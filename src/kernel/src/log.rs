use core::{
    fmt::Write,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::interrupt;

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

pub struct ConsoleWriteError;
pub struct KernelConsole<T: KernelConsoleHardware, Level: MessageLevel> {
    inner: &'static KernelConsoleInner,
    hardware: T,
    lock: crate::spinlock::Spinlock<()>,
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

impl<T: KernelConsoleHardware> core::fmt::Write for KernelConsole<T, EmergencyMessage> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let _ = self.write(s.as_bytes(), KernelConsoleWriteFlags::empty());
        Ok(())
    }
}

impl<T: KernelConsoleHardware> core::fmt::Write for KernelConsole<T, NormalMessage> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let _ = self.write(s.as_bytes(), KernelConsoleWriteFlags::empty());
        Ok(())
    }
}

bitflags::bitflags! {
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

/*
fn is_empty(s: u64) -> bool {
    read_head(s) == write_head(s)
}

fn is_full(s: u64) -> bool {
    (write_head(s) + 1) % (KEC_BUFFER_LEN as u64) == read_head(s)
}
*/
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

    fn write_buffer(
        &self,
        data: &[u8],
        flags: KernelConsoleWriteFlags,
    ) -> Result<(), ConsoleWriteError> {
        let data = &data[0..core::cmp::min(data.len(), MAX_SINGLE_WRITE)];

        loop {
            let state = self.state.load(Ordering::SeqCst);
            let (ok, new_state, copy_offset) = reserve_space(
                state,
                data.len(),
                flags.contains(KernelConsoleWriteFlags::DISCARD_ON_FULL),
            );
            if !ok {
                return Err(ConsoleWriteError {});
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

impl<T: KernelConsoleHardware> KernelConsole<T, EmergencyMessage> {
    pub fn write(
        &self,
        data: &[u8],
        flags: KernelConsoleWriteFlags,
    ) -> Result<(), ConsoleWriteError> {
        self.hardware.write(data, flags);
        self.inner.write_buffer(data, flags)
    }
}

impl<T: KernelConsoleHardware> KernelConsole<T, NormalMessage> {
    pub fn write(
        &self,
        data: &[u8],
        flags: KernelConsoleWriteFlags,
    ) -> Result<(), ConsoleWriteError> {
        self.hardware.write(data, flags);
        self.inner.write_buffer(data, flags)
    }
}

pub fn write_bytes(slice: &[u8], flags: KernelConsoleWriteFlags) -> Result<(), ConsoleWriteError> {
    unsafe { NORMAL_CONSOLE.write(slice, flags) }
}

static mut EMERGENCY_CONSOLE: KernelConsole<
    crate::machine::MachineConsoleHardware,
    EmergencyMessage,
> = KernelConsole {
    inner: &KERNEL_CONSOLE_MAIN,
    hardware: crate::machine::MachineConsoleHardware::new(),
    _pd: core::marker::PhantomData,
    lock: crate::spinlock::Spinlock::new(()),
};

static mut NORMAL_CONSOLE: KernelConsole<crate::machine::MachineConsoleHardware, NormalMessage> =
    KernelConsole {
        inner: &KERNEL_CONSOLE_MAIN,
        hardware: crate::machine::MachineConsoleHardware::new(),
        _pd: core::marker::PhantomData,
        lock: crate::spinlock::Spinlock::new(()),
    };

#[doc(hidden)]
pub fn _print_normal(args: ::core::fmt::Arguments) {
    let istate = interrupt::disable();
    unsafe {
        let _guard = NORMAL_CONSOLE.lock.lock();
        NORMAL_CONSOLE
            .write_fmt(args)
            .expect("printing to serial failed");
    }
    interrupt::set(istate);
}

pub fn _print_emergency(args: ::core::fmt::Arguments) {
    unsafe {
        EMERGENCY_CONSOLE
            .write_fmt(args)
            .expect("printing to serial failed");
    }
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
