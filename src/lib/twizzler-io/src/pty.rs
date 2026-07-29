use std::{
    cell::UnsafeCell,
    io::{ErrorKind, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use libc::{
    _POSIX_VDISABLE, B9600, BRKINT, CREAD, CS8, ECHO, ECHOCTL, ECHOE, ECHOK, ECHOKE, ECHONL, HUPCL,
    ICANON, ICRNL, IEXTEN, IGNCR, IMAXBEL, INLCR, ISIG, ISTRIP, IXANY, IXON, NCCS, NOFLSH, OCRNL,
    ONLCR, OPOST, VDISCARD, VEOF, VEOL, VERASE, VINTR, VKILL, VLNEXT, VMIN, VQUIT, VREPRINT,
    VSTART, VSTATUS, VSTOP, VSUSP, VTIME, VWERASE, XTABS,
};
use memchr::{memchr2, memrchr, memrchr3};
use twizzler::{
    BaseType, Invariant,
    object::{MapFlags, ObjID, Object, ObjectBuilder, TypedObject},
};
use twizzler_abi::syscall::{
    ObjectCreate, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
    ThreadSyncWake, sys_thread_sync,
};

use crate::buffer::VolatileBuffer;

pub const BUF_SZ: usize = 8192;

fn do_sleep(sync: ThreadSyncSleep) -> std::io::Result<()> {
    sys_thread_sync(&mut [ThreadSync::new_sleep(sync)], None)?;
    Ok(())
}

#[derive(Clone)]
struct PtyInputReader {
    pty: Object<PtyBase>,
}

impl Read for PtyInputReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.pty.base().client_input.read_bytes(buf)?;
        if count == 0 && buf.len() > 0 {
            return Err(ErrorKind::WouldBlock.into());
        }
        Ok(count)
    }
}

#[derive(Clone)]
struct PtyOutputWriter {
    pty: Object<PtyBase>,
}

impl Write for PtyOutputWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let count = self.pty.base().client_output.write_bytes(buf)?;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct PtyOutputReader {
    pty: Object<PtyBase>,
}

impl PtyOutputReader {
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.pty.base().client_output.read_bytes(buf)?;
        Ok(count)
    }
}

pub struct PtyClientHandle {
    input: Arc<Mutex<InputConverter<PtyInputReader>>>,
    output: Arc<Mutex<OutputConverter<PtyOutputWriter>>>,
    termios_gen: AtomicU64,
    pty: Object<PtyBase>,
}

impl Clone for PtyClientHandle {
    fn clone(&self) -> Self {
        Self {
            input: self.input.clone(),
            output: self.output.clone(),
            termios_gen: AtomicU64::new(self.termios_gen.load(Ordering::SeqCst)),
            pty: self.pty.clone(),
        }
    }
}

impl PtyClientHandle {
    pub fn new(id: ObjID) -> std::io::Result<Self> {
        let obj =
            unsafe { Object::<PtyBase>::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?;
        let (termios, termios_gen) = obj.base().read_termios();
        Ok(Self {
            input: Arc::new(Mutex::new(InputConverter::new(
                termios,
                PtyInputReader { pty: obj.clone() },
            ))),
            output: Arc::new(Mutex::new(OutputConverter::new(
                termios,
                PtyOutputWriter { pty: obj.clone() },
            ))),
            termios_gen: AtomicU64::new(termios_gen),
            pty: obj,
        })
    }

    fn update_termios(&self) {
        if let Some((termios, termios_gen)) = self
            .pty
            .base()
            .try_read_termios(self.termios_gen.load(Ordering::SeqCst))
        {
            self.input.lock().unwrap().termios = termios;
            self.output.lock().unwrap().termios = termios;
            self.termios_gen.store(termios_gen, Ordering::SeqCst);
        }
    }

    pub fn set_termios(&self, termios: libc::termios) {
        self.pty.base().update_termios(|_| termios);
    }

    pub fn set_winsize(&self, winsize: libc::winsize) {
        self.pty.base().update_winsize(|_| winsize);
    }
}

#[derive(Clone)]
struct PtyInputPoster {
    pty: Object<PtyBase>,
}

impl Write for PtyInputPoster {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let count = self.pty.base().client_input.write_bytes(buf)?;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct PtyServerHandle {
    client_input: Arc<Mutex<InputPoster<PtyInputPoster, PtyOutputWriter>>>,
    client_output: PtyOutputReader,
    termios_gen: AtomicU64,
    signal_handler: Option<fn(&PtyServerHandle, PtySignal)>,
}

impl Write for PtyServerHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_b(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_b()
    }
}

impl Read for PtyServerHandle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read_b(buf)
    }
}

impl Write for PtyClientHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_b(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_b()
    }
}

impl Read for PtyClientHandle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read_b(buf)
    }
}

impl Clone for PtyServerHandle {
    fn clone(&self) -> Self {
        Self {
            client_input: self.client_input.clone(),
            client_output: self.client_output.clone(),
            termios_gen: AtomicU64::new(self.termios_gen.load(Ordering::SeqCst)),
            signal_handler: self.signal_handler,
        }
    }
}

impl PtyServerHandle {
    pub fn new(
        id: ObjID,
        signal_handler: Option<fn(&PtyServerHandle, PtySignal)>,
    ) -> std::io::Result<Self> {
        let obj =
            unsafe { Object::<PtyBase>::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?;
        let (termios, termios_gen) = obj.base().read_termios();
        Ok(Self {
            client_input: Arc::new(Mutex::new(InputPoster::new(
                termios,
                PtyInputPoster { pty: obj.clone() },
                PtyOutputWriter { pty: obj.clone() },
            ))),
            termios_gen: AtomicU64::new(termios_gen),
            client_output: PtyOutputReader { pty: obj },
            signal_handler,
        })
    }

    pub fn object(&self) -> &Object<PtyBase> {
        &self.client_output.pty
    }

    fn update_termios(&self) {
        if let Some((termios, termios_gen)) = self
            .client_output
            .pty
            .base()
            .try_read_termios(self.termios_gen.load(Ordering::SeqCst))
        {
            self.client_input.lock().unwrap().termios = termios;
            self.termios_gen.store(termios_gen, Ordering::SeqCst);
        }
    }

    pub fn set_termios(&self, termios: libc::termios) {
        self.client_output.pty.base().update_termios(|_| termios);
    }

    pub fn set_winsize(&self, winsize: libc::winsize) {
        let old = self.client_output.pty.base().read_winsize().0;
        if old.ws_row != winsize.ws_row
            || old.ws_col != winsize.ws_col
            || old.ws_xpixel != winsize.ws_xpixel
            || old.ws_ypixel != winsize.ws_ypixel
        {
            self.client_output.pty.base().update_winsize(|_| winsize);
            if let Some(signal_handler) = self.signal_handler {
                (signal_handler)(self, PtySignal::Winch);
            }
        }
    }

    pub fn waitpoint(&self, write: bool) -> ThreadSyncSleep {
        if write {
            self.client_output
                .pty
                .base()
                .client_input
                .sync_for_avail_space()
        } else {
            self.client_output
                .pty
                .base()
                .client_output
                .sync_for_pending_data()
        }
    }

    pub fn is_ready(&self, write: bool) -> bool {
        if write {
            self.client_output.pty.base().client_input.avail_space() > 0
        } else {
            !self.client_output.pty.base().client_output.is_empty()
        }
    }
}

impl PtyServerHandle {
    /// POSIX: recognizing INTR/QUIT/SUSP discards pending input unless NOFLSH.
    ///
    /// This only drops what is still in the shared ring. Bytes the client has already
    /// pulled into its canonical line buffer are on the far side of the object and
    /// cannot be reached from here, so the flush is necessarily partial.
    fn flush_input(&self) {
        let mut buf = [0u8; 256];
        let base = self.client_output.pty.base();
        while base.client_input.read_bytes(&mut buf).unwrap_or(0) > 0 {}
    }

    fn dispatch_signal(&self, report: &WriteReport) {
        let Some(signal) = report.posted_signal else {
            return;
        };
        let noflsh = self.client_input.lock().unwrap().termios.c_lflag & NOFLSH != 0;
        if !noflsh
            && matches!(
                signal,
                PtySignal::Interrupt | PtySignal::Quit | PtySignal::Suspend
            )
        {
            self.flush_input();
        }
        if let Some(signal_handler) = self.signal_handler {
            (signal_handler)(self, signal);
        }
    }

    pub fn write_nb(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.update_termios();
        let report = self.client_input.lock().unwrap().write_input(buf)?;
        self.dispatch_signal(&report);
        if report.consumed == 0 && buf.len() > 0 {
            return Err(ErrorKind::WouldBlock.into());
        }
        Ok(report.consumed)
    }

    pub fn read_nb(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.update_termios();
        let count = self.client_output.read(buf)?;
        if count == 0 && buf.len() > 0 {
            return Err(ErrorKind::WouldBlock.into());
        }
        Ok(count)
    }
}

impl PtyServerHandle {
    pub fn get_termios(&self) -> libc::termios {
        self.client_output.pty.base().read_termios().0
    }

    pub fn get_winsize(&self) -> libc::winsize {
        self.client_output.pty.base().read_winsize().0
    }

    pub fn write_b(&self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            self.update_termios();
            let sync = self
                .client_output
                .pty
                .base()
                .client_input
                .sync_for_avail_space();
            let report = self.client_input.lock().unwrap().write_input(buf)?;
            self.dispatch_signal(&report);
            if report.consumed > 0 || buf.len() == 0 {
                return Ok(report.consumed);
            }
            if !self.is_ready(true) {
                do_sleep(sync)?;
            }
        }
    }

    pub fn flush_b(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl PtyServerHandle {
    pub fn read_b(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            self.update_termios();
            let sync = self
                .client_output
                .pty
                .base()
                .client_output
                .sync_for_pending_data();
            let count = self.client_output.read(buf)?;
            if count > 0 || buf.len() == 0 {
                return Ok(count);
            }
            if !self.is_ready(false) {
                do_sleep(sync)?;
            }
        }
    }
}

impl PtyClientHandle {
    pub fn waitpoint(&self, write: bool) -> ThreadSyncSleep {
        if write {
            self.pty.base().client_output.sync_for_avail_space()
        } else {
            self.pty.base().client_input.sync_for_pending_data()
        }
    }

    pub fn is_ready(&self, write: bool) -> bool {
        if write {
            self.pty.base().client_output.avail_space() > 0
        } else {
            !self.pty.base().client_input.is_empty()
        }
    }

    pub fn write_nb(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.update_termios();
        let count = self.output.lock().unwrap().write(buf)?;
        if count == 0 && buf.len() > 0 {
            return Err(ErrorKind::WouldBlock.into());
        }
        Ok(count)
    }

    pub fn read_nb(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.update_termios();
        let res = self.input.lock().unwrap().read(buf);
        match res {
            Ok(c) => Ok(c),
            Err(e) if e.kind() != ErrorKind::WouldBlock => Err(e),
            _ => {
                if buf.len() == 0 {
                    return Ok(0);
                }
                Err(ErrorKind::WouldBlock.into())
            }
        }
    }
}

impl PtyClientHandle {
    pub fn object(&self) -> Object<PtyBase> {
        self.pty.clone()
    }

    pub fn write_b(&self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            self.update_termios();
            let sync = self.pty.base().client_output.sync_for_avail_space();
            let count = self.output.lock().unwrap().write(buf)?;
            if count > 0 || buf.len() == 0 {
                return Ok(count);
            }
            if !self.is_ready(true) {
                do_sleep(sync)?;
            }
        }
    }

    pub fn flush_b(&mut self) -> std::io::Result<()> {
        self.update_termios();
        self.output.lock().unwrap().flush()
    }
}

impl PtyClientHandle {
    pub fn get_termios(&self) -> libc::termios {
        self.pty.base().read_termios().0
    }

    pub fn get_winsize(&self) -> libc::winsize {
        self.pty.base().read_winsize().0
    }

    pub fn read_b(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            self.update_termios();
            let sync = self.pty.base().client_input.sync_for_pending_data();
            let res = self.input.lock().unwrap().read(buf);
            match res {
                Ok(c) => return Ok(c),
                Err(e) if e.kind() != ErrorKind::WouldBlock => return Err(e),
                _ => {
                    if buf.len() == 0 {
                        return Ok(0);
                    }
                    if !self.is_ready(false) {
                        do_sleep(sync)?;
                    }
                }
            }
        }
    }
}

#[derive(Invariant, BaseType)]
pub struct PtyBase {
    termios_gen: AtomicU64,
    termios: UnsafeCell<libc::termios>,
    winsize_gen: AtomicU64,
    winsize: UnsafeCell<libc::winsize>,
    client_input: VolatileBuffer<BUF_SZ>,
    client_output: VolatileBuffer<BUF_SZ>,
}

unsafe impl Send for PtyBase {}
unsafe impl Sync for PtyBase {}

const fn ctrl(x: u8) -> u8 {
    x & 0o37
}

const CEOF: u8 = ctrl(b'd');
const CEOL: u8 = _POSIX_VDISABLE;
const CERASE: u8 = 127;
const CINTR: u8 = ctrl(b'c');
const CSTATUS: u8 = ctrl(b't');
const CKILL: u8 = ctrl(b'u');
const CMIN: u8 = 1;
const CQUIT: u8 = 0o034; // FS, ^\
const CSUSP: u8 = ctrl(b'z');
const CTIME: u8 = 0;
const _CDSUSP: u8 = ctrl(b'y');
const CSTART: u8 = ctrl(b'q');
const CSTOP: u8 = ctrl(b's');
const CLNEXT: u8 = ctrl(b'v');
const CDISCARD: u8 = ctrl(b'o');
const CWERASE: u8 = ctrl(b'w');
const CREPRINT: u8 = ctrl(b'r');
const _CEOT: u8 = CEOF;
const _CBRK: u8 = CEOL;
const _CRPRNT: u8 = CREPRINT;
const _CFLUSH: u8 = CDISCARD;

/// Default control characters, indexed by the `V*` constants. Any slot left at
/// `_POSIX_VDISABLE` is a character we do not recognize.
const DEFAULT_CC: [libc::cc_t; NCCS] = {
    let mut cc = [_POSIX_VDISABLE; NCCS];
    cc[VINTR] = CINTR;
    cc[VQUIT] = CQUIT;
    cc[VERASE] = CERASE;
    cc[VKILL] = CKILL;
    cc[VEOF] = CEOF;
    cc[VTIME] = CTIME;
    cc[VMIN] = CMIN;
    cc[VSTART] = CSTART;
    cc[VSTOP] = CSTOP;
    cc[VSUSP] = CSUSP;
    cc[VEOL] = CEOL;
    cc[VREPRINT] = CREPRINT;
    cc[VDISCARD] = CDISCARD;
    cc[VWERASE] = CWERASE;
    cc[VLNEXT] = CLNEXT;
    cc[VSTATUS] = CSTATUS;
    cc
};

pub const DEFAULT_TERMIOS: libc::termios = libc::termios {
    c_iflag: BRKINT | ISTRIP | ICRNL | IMAXBEL | IXON | IXANY,
    c_oflag: OPOST | ONLCR | XTABS,
    c_cflag: CREAD | CS8 | HUPCL,
    c_lflag: ECHO | ICANON | ISIG | IEXTEN | ECHOE | ECHOK | ECHOKE | ECHOCTL,
    c_cc: DEFAULT_CC,
    __c_ispeed: B9600,
    __c_ospeed: B9600,
    c_line: 0,
};

pub const DEFAULT_TERMIOS_RAW: libc::termios = libc::termios {
    c_iflag: 0,
    c_oflag: 0,
    c_cflag: CREAD | CS8,
    c_lflag: 0,
    c_cc: DEFAULT_CC,
    __c_ispeed: B9600,
    __c_ospeed: B9600,
    c_line: 0,
};

/// Read a control character out of `c_cc`, returning `None` if it is disabled.
///
/// This matters because `_POSIX_VDISABLE` is `0`: searching for a disabled character
/// naively would match every NUL byte in the stream.
fn cc_enabled(termios: &libc::termios, idx: usize) -> Option<u8> {
    let c = termios.c_cc[idx];
    (c != _POSIX_VDISABLE).then_some(c)
}

/// Index of the first byte matching any of the (possibly disabled) control characters.
fn find_cc(buf: &[u8], chars: [Option<u8>; 3]) -> Option<usize> {
    buf.iter()
        .position(|b| chars.iter().any(|c| *c == Some(*b)))
}

/// The signal, if any, that `b` generates. Signal generation is conditional on `ISIG`;
/// with it clear the character must reach the reader untouched (this is what raw-mode
/// line editors rely on to see `^C`).
fn signal_for(termios: &libc::termios, b: u8) -> Option<PtySignal> {
    if termios.c_lflag & ISIG == 0 || b == _POSIX_VDISABLE {
        return None;
    }
    let cc = &termios.c_cc;
    if b == cc[VINTR] {
        Some(PtySignal::Interrupt)
    } else if b == cc[VQUIT] {
        Some(PtySignal::Quit)
    } else if b == cc[VSUSP] {
        Some(PtySignal::Suspend)
    } else if b == cc[VSTATUS] {
        Some(PtySignal::Status)
    } else {
        None
    }
}

impl PtyBase {
    pub fn new(termios: libc::termios) -> Self {
        Self {
            termios_gen: AtomicU64::new(0),
            termios: UnsafeCell::new(termios),
            winsize_gen: AtomicU64::new(0),
            // Start at a conventional size rather than 0x0: a caller that never issues
            // TIOCSWINSZ should still see something a full-screen program can use.
            winsize: UnsafeCell::new(libc::winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            }),
            client_input: VolatileBuffer::new(),
            client_output: VolatileBuffer::new(),
        }
    }

    pub fn create_object(
        spec: ObjectCreate,
        termios: libc::termios,
    ) -> std::io::Result<Object<Self>> {
        let obj = ObjectBuilder::new(spec).build(PtyBase::new(termios))?;
        Ok(obj)
    }

    pub fn update_termios(
        &self,
        mut f: impl FnMut(libc::termios) -> libc::termios,
    ) -> libc::termios {
        loop {
            let current_gen = self.termios_gen.load(std::sync::atomic::Ordering::Acquire);

            // If someone else has the write lock, wait and retry.
            if current_gen & 1 != 0 {
                self.do_sleep_for_termios_gen(current_gen);
                continue;
            }
            if self
                .termios_gen
                .compare_exchange(
                    current_gen,
                    current_gen + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                // We now have the write lock.
                let termios = unsafe { self.termios.get().read() };
                let new_termios = f(termios);
                unsafe { self.termios.get().write(new_termios) };
                self.termios_gen
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.do_wake_for_termios_gen();
                return new_termios;
            }
        }
    }

    fn do_wake_for_termios_gen(&self) {
        let _ = twizzler_abi::syscall::sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&self.termios_gen),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::error!("failed to wake on termios for pty: {}", e));
    }

    fn do_sleep_for_termios_gen(&self, generation: u64) {
        let _ = twizzler_abi::syscall::sys_thread_sync(
            &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&self.termios_gen),
                generation,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))],
            None,
        )
        .inspect_err(|e| tracing::error!("failed to wait on termios for pty: {}", e));
    }

    pub fn try_read_termios(&self, current: u64) -> Option<(libc::termios, u64)> {
        let current_gen = self.termios_gen.load(std::sync::atomic::Ordering::Acquire);
        if current == current_gen {
            return None;
        }
        let val = unsafe { self.termios.get().read() };
        let after_gen = self.termios_gen.load(std::sync::atomic::Ordering::SeqCst);

        if current_gen == after_gen && current_gen & 1 == 0 {
            return Some((val, current_gen));
        }
        None
    }

    pub fn read_termios(&self) -> (libc::termios, u64) {
        loop {
            let current_gen = self.termios_gen.load(std::sync::atomic::Ordering::Acquire);
            let val = unsafe { self.termios.get().read() };
            let after_gen = self.termios_gen.load(std::sync::atomic::Ordering::SeqCst);

            if current_gen == after_gen && current_gen & 1 == 0 {
                return (val, current_gen);
            }
            self.do_sleep_for_termios_gen(after_gen);
        }
    }

    pub fn wait_termios(&self, generation: u64) -> u64 {
        let g = self.termios_gen.load(std::sync::atomic::Ordering::SeqCst);
        if g != generation {
            return g;
        }
        self.do_sleep_for_termios_gen(generation);
        self.termios_gen.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn update_winsize(
        &self,
        mut f: impl FnMut(libc::winsize) -> libc::winsize,
    ) -> libc::winsize {
        loop {
            let current_gen = self.winsize_gen.load(std::sync::atomic::Ordering::Acquire);

            if current_gen & 1 != 0 {
                self.do_sleep_for_winsize_gen(current_gen);
                continue;
            }
            if self
                .winsize_gen
                .compare_exchange(
                    current_gen,
                    current_gen + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                let winsize = unsafe { self.winsize.get().read() };
                let new_winsize = f(winsize);
                unsafe { self.winsize.get().write(new_winsize) };
                self.winsize_gen
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.do_wake_for_winsize_gen();
                return new_winsize;
            }
        }
    }

    fn do_wake_for_winsize_gen(&self) {
        let _ = twizzler_abi::syscall::sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&self.winsize_gen),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::error!("failed to wake on winsize for pty: {}", e));
    }

    fn do_sleep_for_winsize_gen(&self, generation: u64) {
        let _ = twizzler_abi::syscall::sys_thread_sync(
            &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&self.winsize_gen),
                generation,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))],
            None,
        )
        .inspect_err(|e| tracing::error!("failed to wait on winsize for pty: {}", e));
    }

    pub fn try_read_winsize(&self, current: u64) -> Option<(libc::winsize, u64)> {
        let current_gen = self.winsize_gen.load(std::sync::atomic::Ordering::Acquire);
        if current == current_gen {
            return None;
        }
        let val = unsafe { self.winsize.get().read() };
        let after_gen = self.winsize_gen.load(std::sync::atomic::Ordering::SeqCst);

        if current_gen == after_gen && current_gen & 1 == 0 {
            return Some((val, current_gen));
        }
        None
    }

    pub fn read_winsize(&self) -> (libc::winsize, u64) {
        loop {
            let current_gen = self.winsize_gen.load(std::sync::atomic::Ordering::Acquire);
            let val = unsafe { self.winsize.get().read() };
            let after_gen = self.winsize_gen.load(std::sync::atomic::Ordering::SeqCst);

            if current_gen == after_gen && current_gen & 1 == 0 {
                return (val, current_gen);
            }
            self.do_sleep_for_winsize_gen(after_gen);
        }
    }
}

#[derive(Clone)]
pub struct InputPoster<W: Write, E: Write> {
    termios: libc::termios,
    writer: W,
    echoer: E,
    echobuf: [u8; BUF_SZ],
    echobuf_len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtySignal {
    Interrupt,
    Quit,
    Suspend,
    Status,
    Winch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteReport {
    pub consumed: usize,
    pub posted_signal: Option<PtySignal>,
}

impl<W: Write, E: Write> InputPoster<W, E> {
    pub fn new(termios: libc::termios, writer: W, echoer: E) -> Self {
        Self {
            termios,
            writer,
            echoer,
            echobuf: [0; _],
            echobuf_len: 0,
        }
    }

    fn maybe_echo(&mut self, mut buf: &[u8]) -> std::io::Result<()> {
        let canon = self.termios.c_lflag & ICANON != 0;
        let echo = self.termios.c_lflag & ECHO != 0;
        let echoe = self.termios.c_lflag & ECHOE != 0 && canon;
        // Accept either flag: ECHOK erases the line, ECHOKE erases it character-by-
        // character. We only implement the latter rendering, but both mean "show it".
        let echok = self.termios.c_lflag & (ECHOK | ECHOKE) != 0 && canon;
        let echonl = self.termios.c_lflag & ECHONL != 0 && canon;

        // Read the erase characters from c_cc rather than assuming the defaults, so this
        // agrees with InputConverter::refill_linebuf when a program reassigns them.
        let verase = cc_enabled(&self.termios, VERASE);
        let vkill = cc_enabled(&self.termios, VKILL);
        let vwerase = cc_enabled(&self.termios, VWERASE);

        if !echo && !echonl {
            return Ok(());
        }

        if !echo {
            self.echobuf_len = 0;
            for _ in 0..buf.iter().filter(|p| **p == b'\n').count() {
                self.echoer.write_all(&[b'\n'])?;
            }
            return Ok(());
        }

        while buf.len() > 0 {
            // If we overrun the buffer, give up.
            if self.echobuf_len == BUF_SZ {
                self.echobuf_len = 0;
            }

            let thislen = (BUF_SZ - self.echobuf_len).min(buf.len());
            self.echobuf[self.echobuf_len..(self.echobuf_len + thislen)]
                .copy_from_slice(&buf[0..thislen]);

            let mut cur_echo_off = self.echobuf_len;
            self.echobuf_len += thislen;

            while cur_echo_off < self.echobuf_len {
                let echobuf = &self.echobuf[cur_echo_off..self.echobuf_len];
                let erase_idx = find_cc(echobuf, [verase, vkill, vwerase]);
                let nl_idx = memchr::memchr(b'\n', echobuf);
                let min_idx = if let Some(e) = erase_idx
                    && let Some(n) = nl_idx
                {
                    Some(e.min(n))
                } else {
                    erase_idx.or(nl_idx)
                };

                let erase_chars = |this: &mut Self, erase_start: usize, erase_char: usize| {
                    this.echobuf.copy_within((erase_char + 1).., erase_start);
                    this.echobuf_len = this
                        .echobuf_len
                        .saturating_sub((erase_char + 1) - erase_start);
                };

                let echolen = if let Some(idx) = min_idx {
                    if idx > 0 {
                        self.echoer.write_all(&echobuf[0..idx])?;
                    }
                    let c = echobuf[idx];
                    if Some(c) == verase && echoe {
                        self.echoer.write_all(&[8, b' ', 8])?;
                        erase_chars(
                            self,
                            (cur_echo_off + idx).saturating_sub(1),
                            cur_echo_off + idx,
                        );
                    } else if Some(c) == vkill && echok {
                        let idx = idx + cur_echo_off;
                        let space = memrchr(b'\n', &self.echobuf[0..idx]).unwrap_or(0);
                        for _ in 0..(idx.saturating_sub(space + 1)).max(1) {
                            self.echoer.write_all(&[8, b' ', 8])?;
                        }
                        if space + 1 == idx {
                            erase_chars(self, space, idx);
                        } else {
                            erase_chars(self, space + 1, idx);
                        }
                    } else if Some(c) == vwerase && echoe {
                        let idx = idx + cur_echo_off;
                        let space =
                            memrchr3(b'\n', b'\t', b' ', &self.echobuf[0..idx]).unwrap_or(0);
                        for _ in 0..(idx.saturating_sub(space + 1)).max(1) {
                            self.echoer.write_all(&[8, b' ', 8])?;
                        }
                        if space + 1 == idx {
                            erase_chars(self, space, idx);
                        } else {
                            erase_chars(self, space + 1, idx);
                        }
                    } else if c == b'\n' {
                        self.echoer.write_all(&[c])?;
                        self.echobuf_len = 0;
                    } else {
                        self.echoer.write_all(&[c])?;
                    }
                    idx + 1
                } else {
                    self.echoer.write_all(echobuf)?;
                    echobuf.len()
                };
                cur_echo_off += echolen;
            }

            buf = &buf[thislen..];
        }
        Ok(())
    }

    /// Echo the bytes we just accepted, after applying the same input mapping the client
    /// will apply when it reads them, so what the user sees matches what the client gets.
    fn echo_input(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let mut off = 0;
        while off < buf.len() {
            let mut echobuf = [0u8; BUF_SZ];
            let chunk = BUF_SZ.min(buf.len() - off);
            echobuf[0..chunk].copy_from_slice(&buf[off..(off + chunk)]);
            // input_map can shrink the data (IGNCR), so advance by the input length --
            // advancing by the mapped length fails to make progress when it maps to zero.
            let mapped = input_map(&self.termios, &mut echobuf[0..chunk]);
            self.maybe_echo(&echobuf[0..mapped])?;
            off += chunk;
        }
        Ok(())
    }

    /// With ECHOCTL, a signal-generating control character echoes as `^X`.
    fn echo_signal_char(&mut self, c: u8) -> std::io::Result<()> {
        let want = ECHO | ECHOCTL;
        if self.termios.c_lflag & want != want {
            return Ok(());
        }
        if c < 0x20 || c == 0x7f {
            self.echoer.write_all(&[b'^', c ^ 0x40])?;
        }
        Ok(())
    }

    pub fn write_input(&mut self, mut buf: &[u8]) -> std::io::Result<WriteReport> {
        let termios = self.termios;
        let mut total = 0;
        let mut sig = None;

        while buf.len() > 0 && sig.is_none() {
            let hit = buf.iter().position(|b| signal_for(&termios, *b).is_some());
            let count = hit.unwrap_or(buf.len());

            let wcount = self.writer.write(&buf[0..count])?;
            self.echo_input(&buf[0..wcount])?;
            total += wcount;
            buf = &buf[wcount..];

            // Only recognize the signal once everything ahead of it has been consumed.
            // Reporting it after a short write would leave the character in place, and
            // the caller's retry would post the same signal a second time.
            if wcount < count {
                break;
            }
            if hit.is_some() {
                let c = buf[0];
                self.echo_signal_char(c)?;
                sig = signal_for(&termios, c);
                total += 1;
                buf = &buf[1..];
            }
        }

        Ok(WriteReport {
            consumed: total,
            posted_signal: sig,
        })
    }
}

#[derive(Clone)]
pub struct OutputConverter<W: Write> {
    termios: libc::termios,
    writer: W,
    /// Tail of a translated sequence (the "\n" of a CRLF) that did not fit in the output
    /// buffer. Emitted ahead of anything else so the sequence is not torn across calls.
    pending: [u8; 2],
    pending_len: usize,
}

impl<W: Write> OutputConverter<W> {
    pub fn new(termios: libc::termios, writer: W) -> Self {
        Self {
            termios,
            writer,
            pending: [0; 2],
            pending_len: 0,
        }
    }

    /// Push out any leftover tail. Returns false if it still does not fit, in which case
    /// the caller must not accept new input.
    fn flush_pending(&mut self) -> std::io::Result<bool> {
        while self.pending_len > 0 {
            let n = self.writer.write(&self.pending[0..self.pending_len])?;
            if n == 0 {
                return Ok(false);
            }
            self.pending.copy_within(n.., 0);
            self.pending_len -= n;
        }
        Ok(true)
    }

    pub fn write_bytes_simple(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }

    pub fn write_bytes_processed(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
        if !self.flush_pending()? {
            return Ok(0);
        }

        let cr_to_nl = self.termios.c_oflag & OCRNL != 0;
        let nl_to_crnl = self.termios.c_oflag & ONLCR != 0;

        if !cr_to_nl && !nl_to_crnl {
            return self.write_bytes_simple(buf);
        }

        let mut total = 0;
        while buf.len() > 0 {
            let (count, extra) = if let Some(idx) = memchr2(b'\r', b'\n', buf) {
                match buf[idx] {
                    b'\r' if cr_to_nl => {
                        if nl_to_crnl {
                            (idx, Some(b"\r\n" as &[u8]))
                        } else {
                            (idx, Some(b"\n" as &[u8]))
                        }
                    }
                    b'\n' if nl_to_crnl => (idx, Some(b"\r\n" as &[u8])),
                    _ => (idx + 1, None),
                }
            } else {
                (buf.len(), None)
            };
            let thiswrite = self.writer.write(&buf[0..count])?;
            total += thiswrite;
            buf = &buf[thiswrite..];
            // Short write: stop rather than consuming input we never wrote.
            if thiswrite < count {
                break;
            }
            if let Some(extra) = extra {
                let n = self.writer.write(extra)?;
                if n < extra.len() {
                    let rest = &extra[n..];
                    self.pending[0..rest.len()].copy_from_slice(rest);
                    self.pending_len = rest.len();
                }
                // Note: we only increment by 1 here because regardless of the extra
                // data we write, it came from 1 byte of the input buffer.
                total += 1;
                buf = &buf[1..];
                if self.pending_len > 0 {
                    break;
                }
            }
        }

        Ok(total)
    }
}

impl<W: Write> Write for OutputConverter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.termios.c_oflag & OPOST != 0 {
            self.write_bytes_processed(buf)
        } else {
            if !self.flush_pending()? {
                return Ok(0);
            }
            self.write_bytes_simple(buf)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flush_pending()?;
        self.writer.flush()
    }
}

#[derive(Clone)]
pub struct InputConverter<R: Read> {
    termios: libc::termios,
    linebuf: [u8; BUF_SZ],
    linebuf_count: usize,
    reader: R,
}

impl<R: Read> InputConverter<R> {
    pub fn new(termios: libc::termios, reader: R) -> Self {
        Self {
            termios,
            reader,
            linebuf_count: 0,
            linebuf: [0; BUF_SZ],
        }
    }

    fn refill_linebuf(&mut self) -> std::io::Result<()> {
        let start = self.linebuf_count;
        let count = self.reader.read(&mut self.linebuf[start..])?;
        let count = input_map(&self.termios, &mut self.linebuf[start..(start + count)]);
        self.linebuf_count = start + count;

        let verase = cc_enabled(&self.termios, VERASE);
        let vwerase = cc_enabled(&self.termios, VWERASE);
        let vkill = cc_enabled(&self.termios, VKILL);

        // Apply every erase character in the newly-read region, not just the first: each
        // one removes itself plus the range it erases, so the scan resumes where it landed.
        let mut idx = start;
        while idx < self.linebuf_count {
            let c = self.linebuf[idx];
            let back_to = if Some(c) == verase {
                if idx > 0 && self.linebuf[idx - 1] != b'\n' {
                    idx - 1
                } else {
                    idx
                }
            } else if Some(c) == vwerase {
                memrchr3(b'\n', b' ', b'\t', &self.linebuf[0..idx])
                    .map(|i| i + 1)
                    .unwrap_or(0)
            } else if Some(c) == vkill {
                memrchr(b'\n', &self.linebuf[0..idx])
                    .map(|i| i + 1)
                    .unwrap_or(0)
            } else {
                idx += 1;
                continue;
            };

            self.linebuf.copy_within((idx + 1).., back_to);
            // The erase consumes the erased range *and* the erase character itself.
            self.linebuf_count -= (idx + 1) - back_to;
            idx = back_to;
        }

        Ok(())
    }

    fn drain_linebuf(&mut self, buf: &mut [u8]) -> (usize, bool) {
        // Search the whole buffered region, not just the first buf.len() bytes: a line
        // longer than the caller's buffer is still a complete line, and treating it as
        // incomplete would send read_canon back to the reader and block forever.
        let (nl, eof) = {
            let pending = &self.linebuf[0..self.linebuf_count];
            let veof = cc_enabled(&self.termios, VEOF);
            (
                memchr::memchr(b'\n', pending),
                veof.and_then(|c| memchr::memchr(c, pending)),
            )
        };

        // An EOF ahead of any newline terminates the line, and is itself not delivered.
        let eof = match (nl, eof) {
            (Some(n), Some(e)) if e >= n => None,
            (_, e) => e,
        };

        let (line_len, has_eof) = match (nl, eof) {
            (_, Some(e)) => (e, true),
            (Some(n), None) => (n + 1, false),
            // No line terminator yet; only a full buffer forces a delivery.
            (None, None) if self.linebuf_count < BUF_SZ => return (0, false),
            (None, None) => (self.linebuf_count, false),
        };

        // A short read is fine -- the rest of the line stays buffered for the next call.
        let count = buf.len().min(line_len);
        buf[0..count].copy_from_slice(&self.linebuf[0..count]);
        // Drop what we delivered, and the EOF marker too once the whole line is gone.
        // Stripping it earlier would lose the boundary for the remainder of the line.
        let drop = if has_eof && count == line_len {
            count + 1
        } else {
            count
        };
        self.linebuf.copy_within(drop.., 0);
        self.linebuf_count -= drop;
        (count, true)
    }

    pub fn read_canon(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total = 0;
        while buf.len() > 0 {
            let before = self.linebuf_count;
            self.refill_linebuf()?;
            if self.linebuf_count == 0 {
                if total == 0 {
                    return Err(ErrorKind::WouldBlock.into());
                }
                return Ok(total);
            }

            let (count, end) = self.drain_linebuf(buf);

            buf = &mut buf[count..];
            total += count;
            if end {
                return Ok(total);
            }
            // Partial line and the reader had nothing new: don't spin waiting for a
            // terminator that can only arrive on a later call.
            if self.linebuf_count == before {
                if total == 0 {
                    return Err(ErrorKind::WouldBlock.into());
                }
                return Ok(total);
            }
        }
        Ok(total)
    }

    pub fn pending_linebuf(&self) -> usize {
        self.linebuf_count
    }

    pub fn read_raw(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total = 0;
        while buf.len() > 0 {
            let thisread = match self.reader.read(buf) {
                Ok(l) => l,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if total > 0 {
                        return Ok(total);
                    } else {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            };

            if thisread == 0 {
                return Ok(total);
            }

            // this might squash characters
            let thisread = input_map(&self.termios, &mut buf[0..thisread]);

            total += thisread;
            buf = &mut buf[thisread..];
        }
        Ok(total)
    }
}

fn input_map(termios: &libc::termios, mut buf: &mut [u8]) -> usize {
    let nl_to_cr = termios.c_iflag & INLCR != 0;
    let ignore_cr = termios.c_iflag & IGNCR != 0;
    let cr_to_nl = termios.c_iflag & ICRNL != 0;

    let search_ln = nl_to_cr;
    let search_cr = ignore_cr || cr_to_nl;

    if !search_cr && !search_ln {
        return buf.len();
    }

    let mut total = 0;
    while buf.len() > 0 {
        let idx = if search_ln && search_cr {
            memchr::memchr2(b'\r', b'\n', buf)
        } else if search_cr {
            memchr::memchr(b'\r', buf)
        } else if search_ln {
            memchr::memchr(b'\n', buf)
        } else {
            unreachable!()
        };

        if let Some(idx) = idx {
            let len = match buf[idx] {
                b'\r' if ignore_cr => {
                    buf.copy_within((idx + 1).., idx);
                    let newend = buf.len() - 1;
                    buf = &mut buf[idx..newend];
                    idx
                }
                b'\r' if cr_to_nl => {
                    buf[idx] = b'\n';
                    buf = &mut buf[(idx + 1)..];
                    idx + 1
                }
                // Note a translated character is not re-examined: IGNCR applies only to
                // CRs that arrived as CRs, not to one INLCR just produced.
                b'\n' if nl_to_cr => {
                    buf[idx] = b'\r';
                    buf = &mut buf[(idx + 1)..];
                    idx + 1
                }
                _ => {
                    panic!("unexpected character");
                }
            };
            total += len;
        } else {
            total += buf.len();
            return total;
        }
    }

    total
}

impl<R: Read> Read for InputConverter<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.termios.c_lflag & ICANON != 0 {
            self.read_canon(buf)
        } else {
            self.read_raw(buf)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Seek};

    use libc::{ICANON, ICRNL, IGNCR, INLCR, OCRNL, ONLCR, VEOF, VERASE, VKILL, VWERASE, termios};

    use crate::pty::{CEOF, CKILL, CWERASE, InputConverter, OutputConverter, ctrl};

    fn test_output_processing(oflag: u32, input: &[u8], expected: &[u8]) {
        let t = termios {
            c_iflag: 0,
            c_oflag: oflag,
            c_cflag: 0,
            c_lflag: 0,
            c_cc: [0; _],
            __c_ispeed: 0,
            __c_ospeed: 0,
            c_line: 0,
        };
        let buf = &mut [1u8; 1024] as &mut [u8];
        let mut cursor = Cursor::new(buf);
        let mut converter = OutputConverter::new(t, &mut cursor);
        let _written = converter.write_bytes_processed(&input).unwrap();
        let written = cursor.position() as usize;
        cursor.rewind().unwrap();
        let buf = cursor.get_ref();
        assert_eq!(&buf[0..written], expected);
    }

    fn test_input_processing(iflag: u32, mut input: &[u8], expected: &[u8]) {
        let t = termios {
            c_iflag: iflag,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: 0,
            c_cc: [0; _],
            __c_ispeed: 0,
            __c_ospeed: 0,
            c_line: 0,
        };
        let mut converter = InputConverter::new(t, &mut input);
        let mut buf = [0u8; 1024];
        let read = converter.read_raw(&mut buf).unwrap();
        assert_eq!(&buf[0..read], expected);
    }

    fn test_canon(iflag: u32, mut input: &[u8], expected: &[&[u8]]) {
        let mut t = termios {
            c_iflag: iflag,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: ICANON,
            c_cc: [0; _],
            __c_ispeed: 0,
            __c_ospeed: 0,
            c_line: 0,
        };
        t.c_cc[VEOF] = CEOF; // ^D, 0x04
        t.c_cc[VERASE] = ctrl(b'h'); // ^H, 0x08
        t.c_cc[VKILL] = CKILL; // ^U, 0x15
        t.c_cc[VWERASE] = CWERASE; // ^W, 0x17
        let mut converter = InputConverter::new(t, &mut input);
        for expected in expected {
            let mut buf = [0u8; 1024];
            let read = converter.read_canon(&mut buf).unwrap();
            assert_eq!(&buf[0..read], *expected);
        }
    }

    #[test]
    fn test_raw_input_processing() {
        let input = b"start\ns\rend" as &[u8];
        test_input_processing(0, input, b"start\ns\rend");

        test_input_processing(ICRNL, input, b"start\ns\nend");
        test_input_processing(INLCR, input, b"start\rs\rend");
        test_input_processing(IGNCR, input, b"start\nsend");
        // INLCR turns the NL into a CR; IGNCR does not then re-examine it.
        test_input_processing(IGNCR | INLCR, input, b"start\rsend");
        test_input_processing(IGNCR | ICRNL, input, b"start\nsend");

        let input = b"nothing" as &[u8];
        test_input_processing(ICRNL, input, b"nothing");
        test_input_processing(INLCR, input, b"nothing");
        test_input_processing(IGNCR, input, b"nothing");
        test_input_processing(IGNCR | INLCR, input, b"nothing");
        test_input_processing(IGNCR | ICRNL, input, b"nothing");

        let input = b"\n\r" as &[u8];
        test_input_processing(ICRNL, input, b"\n\n");
        test_input_processing(INLCR, input, b"\r\r");
        test_input_processing(IGNCR, input, b"\n");
        test_input_processing(IGNCR | INLCR, input, b"\r");
        test_input_processing(IGNCR | ICRNL, input, b"\n");
    }

    #[test]
    fn test_canon_input() {
        let input = b"first\nsecond\nthird" as &[u8];
        test_canon(0, input, &[b"first\n", b"second\n"]);

        let input = b"first\nsecond\nthird\n" as &[u8];
        test_canon(0, input, &[b"first\n", b"second\n", b"third\n"]);

        let input = b"first\x04second\n" as &[u8];
        test_canon(0, input, &[b"first", b"second\n"]);

        let input = b"first" as &[u8];
        test_canon(0, input, &[]);

        let input = b"\x04" as &[u8];
        test_canon(0, input, &[]);

        let input = b"test words\x08S\n" as &[u8];
        test_canon(0, input, &[b"test wordS\n"]);

        let input = b"test\n\x08S\n" as &[u8];
        test_canon(0, input, &[b"test\n", b"S\n"]);

        // ^U (VKILL) erases back to the start of the line.
        let input = b"test words\x15S\n" as &[u8];
        test_canon(0, input, &[b"S\n"]);

        let input = b"test\n\x15S\n" as &[u8];
        test_canon(0, input, &[b"test\n", b"S\n"]);

        // ^W (VWERASE) erases back to the start of the current word.
        let input = b"test words\x17S\n" as &[u8];
        test_canon(0, input, &[b"test S\n"]);

        let input = b"test\n\x17S\n" as &[u8];
        test_canon(0, input, &[b"test\n", b"S\n"]);

        // Successive erases each take effect, not just the first.
        let input = b"ab\x08\x08c\n" as &[u8];
        test_canon(0, input, &[b"c\n"]);
    }

    #[test]
    fn test_output() {
        let input = b"start\ns\rend" as &[u8];
        test_output_processing(0, input, b"start\ns\rend");

        test_output_processing(OCRNL, input, b"start\ns\nend");
        test_output_processing(ONLCR, input, b"start\r\ns\rend");
        test_output_processing(ONLCR | OCRNL, input, b"start\r\ns\r\nend");
    }
}
