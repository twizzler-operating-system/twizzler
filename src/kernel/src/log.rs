use core::{
    fmt::Write,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use twizzler_abi::syscall::{KernelConsoleReadFlags, KernelConsoleSource, ThreadSyncSleep};
use twizzler_rt_abi::{
    error::{IoError, TwzError},
    Result,
};

use crate::{condvar::CondVar, interrupt, spinlock::Spinlock};

const KEC_BUFFER_LEN: usize = 4096;
const MAX_SINGLE_WRITE: usize = KEC_BUFFER_LEN / 2;
struct KernelConsoleInner {
    state: AtomicU64,
    buffer: core::cell::UnsafeCell<[u8; KEC_BUFFER_LEN]>,
}
unsafe impl Sync for KernelConsoleInner {}
pub trait MessageLevel {}
pub struct EmergencyMessage;
impl MessageLevel for EmergencyMessage {}
pub struct NormalMessage;
impl MessageLevel for NormalMessage {}

const INPUT_BUFFER_SIZE: usize = 1024;
pub struct KernelConsoleReadBuffer {
    buf: [u8; INPUT_BUFFER_SIZE],
    pos: usize,
}

impl KernelConsoleReadBuffer {
    const fn new() -> Self {
        Self {
            buf: [0; INPUT_BUFFER_SIZE],
            pos: 0,
        }
    }

    fn push_input_byte(&mut self, byte: u8) {
        if self.pos == INPUT_BUFFER_SIZE {
            return;
        }
        self.buf[self.pos] = byte;
        self.pos += 1;
    }

    pub fn read_byte(&mut self) -> Option<u8> {
        if self.pos == 0 {
            return None;
        }
        let byte = self.buf[0];
        self.buf.copy_within(1.., 0);
        self.pos -= 1;
        Some(byte)
    }
}

pub struct KernelConsole<T: KernelConsoleHardware, Level: MessageLevel> {
    inner: &'static KernelConsoleInner,
    hardware: T,
    lock: Spinlock<()>,
    read_lock: Spinlock<KernelConsoleReadBuffer>,
    read_cv: CondVar,
    _pd: core::marker::PhantomData<Level>,
}
unsafe impl<T: KernelConsoleHardware, Level: MessageLevel> Sync for KernelConsole<T, Level> {}

static KERNEL_CONSOLE_MAIN: KernelConsoleInner = KernelConsoleInner {
    state: AtomicU64::new(0),
    buffer: core::cell::UnsafeCell::new([0; KEC_BUFFER_LEN]),
};

pub trait KernelConsoleHardware {
    fn write(&self, data: &[u8], flags: KernelConsoleWriteFlags);
}

struct KernelConsoleRef<T: KernelConsoleHardware + 'static, M: MessageLevel + 'static> {
    console: &'static KernelConsole<T, M>,
}

impl<T: KernelConsoleHardware + 'static, M: MessageLevel + 'static> KernelConsoleRef<T, M> {
    pub fn write(&self, data: &[u8], flags: KernelConsoleWriteFlags) -> Result<()> {
        self.console.hardware.write(data, flags);
        self.console.inner.write_buffer(data, flags)
    }
}

impl<T: KernelConsoleHardware> core::fmt::Write for KernelConsoleRef<T, EmergencyMessage> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let _ = self.write(s.as_bytes(), KernelConsoleWriteFlags::empty());
        Ok(())
    }
}

impl<T: KernelConsoleHardware> core::fmt::Write for KernelConsoleRef<T, NormalMessage> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let _ = self.write(s.as_bytes(), KernelConsoleWriteFlags::empty());
        Ok(())
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy)]
    pub struct KernelConsoleWriteFlags: u32 {
        const DISCARD_ON_FULL = 1;
    }
}

impl From<twizzler_abi::syscall::KernelConsoleWriteFlags> for KernelConsoleWriteFlags {
    fn from(x: twizzler_abi::syscall::KernelConsoleWriteFlags) -> Self {
        if x.contains(twizzler_abi::syscall::KernelConsoleWriteFlags::DISCARD_ON_FULL) {
            Self::DISCARD_ON_FULL
        } else {
            Self::empty()
        }
    }
}

fn write_head(s: u64) -> u64 {
    (s >> 32) & 0xffff
}

fn write_resv(s: u64) -> u64 {
    (s >> 16) & 0xffff
}

fn read_head(s: u64) -> u64 {
    s & 0xffff
}

fn new_state(rh: u64, wh: u64, wr: u64) -> u64 {
    ((rh % KEC_BUFFER_LEN as u64) & 0xffff)
        | (((wh % KEC_BUFFER_LEN as u64) & 0xffff) << 32)
        | (((wr % KEC_BUFFER_LEN as u64) & 0xffff) << 16)
}

fn did_pass(x: u64, y: u64, l: u64, n: u64) -> bool {
    assert!(l < n);
    let next_x = (x + l) % n;
    let did_wrap = next_x < x;
    if x < y {
        did_wrap || next_x >= y
    } else {
        next_x >= y && did_wrap
    }
}

fn reserve_write(state: u64, len: usize) -> u64 {
    let len = len as u64;
    let wr = write_resv(state);
    let mut wh = write_head(state);
    let mut rh = read_head(state);

    let passed_rh = did_pass(wr, rh, len, KEC_BUFFER_LEN as u64);
    let passed_wh = did_pass(wr, wh, len, KEC_BUFFER_LEN as u64);

    let wr = (wr + len) % KEC_BUFFER_LEN as u64;

    if passed_rh {
        rh = wr;
    }

    if passed_wh {
        wh = (wr - len) % KEC_BUFFER_LEN as u64;
    }

    new_state(rh, wh, wr)
}

fn commit_write(state: u64, len: usize) -> u64 {
    let wh = write_head(state);
    let wr = write_resv(state);
    new_state(read_head(state), wh + len as u64, wr)
}

fn reserve_space(state: u64, len: usize, toss: bool) -> (bool, u64, u64) {
    let new_state = reserve_write(state, len);
    (
        read_head(state) == read_head(new_state) || !toss,
        new_state,
        write_head(state),
    )
}

impl KernelConsoleInner {
    fn try_commit(&self, old: u64, new: u64) -> bool {
        self.state
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    fn write_buffer(&self, data: &[u8], flags: KernelConsoleWriteFlags) -> Result<()> {
        let data = &data[0..core::cmp::min(data.len(), MAX_SINGLE_WRITE)];

        loop {
            let state = self.state.load(Ordering::SeqCst);
            let (ok, new_state, copy_offset) = reserve_space(
                state,
                data.len(),
                flags.contains(KernelConsoleWriteFlags::DISCARD_ON_FULL),
            );
            if !ok {
                return Err(TwzError::from(IoError::DataLoss));
            }

            if !self.try_commit(state, new_state) {
                continue;
            }

            let (first_len, second_len) = if copy_offset + data.len() as u64 > KEC_BUFFER_LEN as u64
            {
                let first_len = KEC_BUFFER_LEN as u64 - copy_offset;
                (first_len, data.len() as u64 - first_len)
            } else {
                (data.len() as u64, 0)
            };
            (&mut unsafe { *self.buffer.get() })
                [copy_offset as usize..(copy_offset + first_len) as usize]
                .copy_from_slice(&data[0..first_len as usize]);
            (&mut unsafe { *self.buffer.get() })[0..second_len as usize]
                .copy_from_slice(&data[first_len as usize..(first_len + second_len) as usize]);
            let new_committed_state = commit_write(new_state, data.len());
            if self.try_commit(new_state, new_committed_state) {
                break;
            }
        }
        Ok(())
    }
}

impl<T: KernelConsoleHardware, M: MessageLevel> KernelConsole<T, M> {
    fn read_buffer_bytes(&self, _slice: &mut [u8]) -> Result<usize> {
        todo!()
    }

    fn read_bytes(
        &self,
        slice: &mut [u8],
        flags: KernelConsoleReadFlags,
        timeout: Option<Duration>,
        waiter: Option<ThreadSyncSleep>,
    ) -> Result<usize> {
        const MAX_SINGLE_READ: usize = 128;
        let mut i = 0;
        let mut tmp = [0u8; MAX_SINGLE_READ];
        let mut timedout = false;
        while i < slice.len() {
            let mut reader = self.read_lock.lock();
            match reader.read_byte() {
                Some(x) => {
                    tmp[i] = match x {
                        4 => break,
                        _ => x,
                    };
                    i += 1;
                }
                None => {
                    if flags.contains(KernelConsoleReadFlags::NONBLOCKING)
                        || i > 0
                        || waiter.is_some_and(|w| w.ready())
                    {
                        break;
                    }
                    let (_, to) = self.read_cv.wait_waiters(reader, timeout, waiter);
                    if to {
                        timedout = to;
                        break;
                    }
                }
            }
        }
        if i > 0 {
            (&mut slice[0..i]).copy_from_slice(&tmp[0..i]);
            Ok(i)
        } else if timedout {
            Err(TwzError::TIMED_OUT)
        } else {
            Ok(0)
        }
    }
}

pub fn write_bytes(
    target: KernelConsoleSource,
    slice: &[u8],
    flags: KernelConsoleWriteFlags,
) -> Result<()> {
    let writer = match target {
        KernelConsoleSource::Console => Ok(KernelConsoleRef {
            console: &NORMAL_CONSOLE,
        }),
        KernelConsoleSource::Buffer => Err(TwzError::Io(IoError::DeviceError)),
        KernelConsoleSource::DebugConsole => Ok(KernelConsoleRef {
            console: &DEBUG_CONSOLE,
        }),
    }?;
    writer.write(slice, flags)
}

pub fn read_bytes(
    source: KernelConsoleSource,
    slice: &mut [u8],
    flags: KernelConsoleReadFlags,
    timeout: Option<Duration>,
    waiter: Option<ThreadSyncSleep>,
) -> Result<usize> {
    match source {
        KernelConsoleSource::Console => NORMAL_CONSOLE.read_bytes(slice, flags, timeout, waiter),
        KernelConsoleSource::DebugConsole => {
            DEBUG_CONSOLE.read_bytes(slice, flags, timeout, waiter)
        }
        KernelConsoleSource::Buffer => return Err(IoError::DeviceError.into()),
    }
}

pub fn read_buffer_bytes(slice: &mut [u8]) -> Result<usize> {
    NORMAL_CONSOLE.read_buffer_bytes(slice)
}

pub fn push_input_byte(byte: u8, debug: bool) {
    if debug {
        DEBUG_CONSOLE.read_lock.lock().push_input_byte(byte);
        DEBUG_CONSOLE.read_cv.signal();
        return;
    }
    let byte = match byte {
        13 => 10,
        127 => 8,
        x => x,
    };
    NORMAL_CONSOLE.read_lock.lock().push_input_byte(byte);
    NORMAL_CONSOLE.read_cv.signal();
}

static EMERGENCY_CONSOLE: KernelConsole<crate::machine::MachineConsoleHardware, EmergencyMessage> =
    KernelConsole {
        inner: &KERNEL_CONSOLE_MAIN,
        hardware: crate::machine::MachineConsoleHardware::new(false),
        _pd: core::marker::PhantomData,
        lock: Spinlock::new(()),
        read_lock: Spinlock::new(KernelConsoleReadBuffer::new()),
        read_cv: CondVar::new(),
    };

static NORMAL_CONSOLE: KernelConsole<crate::machine::MachineConsoleHardware, NormalMessage> =
    KernelConsole {
        inner: &KERNEL_CONSOLE_MAIN,
        hardware: crate::machine::MachineConsoleHardware::new(false),
        _pd: core::marker::PhantomData,
        lock: Spinlock::new(()),
        read_lock: Spinlock::new(KernelConsoleReadBuffer::new()),
        read_cv: CondVar::new(),
    };

static DEBUG_CONSOLE: KernelConsole<crate::machine::MachineConsoleHardware, NormalMessage> =
    KernelConsole {
        inner: &KERNEL_CONSOLE_MAIN,
        hardware: crate::machine::MachineConsoleHardware::new(true),
        _pd: core::marker::PhantomData,
        lock: Spinlock::new(()),
        read_lock: Spinlock::new(KernelConsoleReadBuffer::new()),
        read_cv: CondVar::new(),
    };

#[doc(hidden)]
pub fn _print_debug(args: ::core::fmt::Arguments) {
    let istate = interrupt::disable();
    {
        let _guard = DEBUG_CONSOLE.lock.lock();
        let mut writer = KernelConsoleRef {
            console: &DEBUG_CONSOLE,
        };
        writer.write_fmt(args).expect("printing to serial failed");
    }
    interrupt::set(istate);
}

#[doc(hidden)]
pub fn _print_normal(args: ::core::fmt::Arguments) {
    let istate = interrupt::disable();
    {
        let _guard = NORMAL_CONSOLE.lock.lock();
        let mut writer = KernelConsoleRef {
            console: &NORMAL_CONSOLE,
        };
        writer.write_fmt(args).expect("printing to serial failed");
    }
    interrupt::set(istate);
}

pub fn _print_emergency(args: ::core::fmt::Arguments) {
    let istate = interrupt::disable();
    {
        let mut writer = KernelConsoleRef {
            console: &NORMAL_CONSOLE,
        };
        writer.write_fmt(args).expect("printing to serial failed");
    }
    interrupt::set(istate);
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::_print_normal(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! logln {
    () => {
        $crate::log!("\n")
    };
    ($fmt:expr) => {
        $crate::log!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::log!(concat!($fmt, "\n"), $($arg)*)
    };
}

#[macro_export]
macro_rules! emerglog {
    ($($arg:tt)*) => {
        $crate::log::_print_emergency(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! emerglogln {
    () => {
        $crate::emerglog!("\n")
    };
    ($fmt:expr) => {
        $crate::emerglog!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::emerglog!(concat!($fmt, "\n"), $($arg)*)
    };
}
