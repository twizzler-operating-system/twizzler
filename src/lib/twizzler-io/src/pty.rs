use std::{
    cell::UnsafeCell,
    io::{ErrorKind, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use libc::{
    _POSIX_VDISABLE, B9600, BRKINT, CREAD, CS7, ECHO, ECHOCTL, ECHOE, ECHOK, ECHOKE, ECHONL, HUPCL,
    ICANON, ICRNL, IEXTEN, IGNCR, IMAXBEL, INLCR, ISIG, ISTRIP, IXANY, IXON, OCRNL, ONLCR, OPOST,
    PARENB, VEOF, VERASE, VINTR, VKILL, VQUIT, VSTATUS, VWERASE, XTABS,
};
use memchr::{memchr2, memchr3, memrchr, memrchr3};
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
            do_sleep(self.pty.base().client_input.sync_for_pending_data())?;
            return self.read(buf);
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
        if count == 0 && buf.len() > 0 {
            do_sleep(self.pty.base().client_output.sync_for_avail_space())?;
            return self.write(buf);
        }
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

impl Read for PtyOutputReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let count = self.pty.base().client_output.read_bytes(buf)?;
        if count == 0 && buf.len() > 0 {
            do_sleep(self.pty.base().client_output.sync_for_pending_data())?;
            return self.read(buf);
        }
        Ok(count)
    }
}

#[derive(Clone)]
pub struct PtyClientHandle {
    input: Arc<Mutex<InputConverter<PtyInputReader>>>,
    output: OutputConverter<PtyOutputWriter>,
    termios_gen: u64,
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
            output: OutputConverter::new(termios, PtyOutputWriter { pty: obj.clone() }),
            termios_gen,
        })
    }

    fn update_termios(&mut self) {
        if let Some((termios, termios_gen)) = self
            .output
            .writer
            .pty
            .base()
            .try_read_termios(self.termios_gen)
        {
            self.input.lock().unwrap().termios = termios;
            self.output.termios = termios;
            self.termios_gen = termios_gen;
        }
    }

    pub fn set_termios(&self, termios: libc::termios) {
        self.output.writer.pty.base().update_termios(|_| termios);
    }
}

#[derive(Clone)]
struct PtyInputPoster {
    pty: Object<PtyBase>,
}

impl Write for PtyInputPoster {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let count = self.pty.base().client_input.write_bytes(buf)?;
        if count == 0 && buf.len() > 0 {
            let sync = self.pty.base().client_input.sync_for_avail_space();
            do_sleep(sync)?;
            return self.write(buf);
        } else {
            Ok(count)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct PtyServerHandle {
    client_input: Arc<Mutex<InputPoster<PtyInputPoster, PtyOutputWriter>>>,
    client_output: PtyOutputReader,
    termios_gen: u64,
    signal_handler: Option<fn(&PtyServerHandle, PtySignal)>,
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
            termios_gen,
            client_output: PtyOutputReader { pty: obj },
            signal_handler,
        })
    }

    fn update_termios(&mut self) {
        if let Some((termios, termios_gen)) = self
            .client_output
            .pty
            .base()
            .try_read_termios(self.termios_gen)
        {
            self.client_input.lock().unwrap().termios = termios;
            self.termios_gen = termios_gen;
        }
    }

    pub fn object(&self) -> &Object<PtyBase> {
        &self.client_output.pty
    }

    pub fn set_termios(&self, termios: libc::termios) {
        self.client_output.pty.base().update_termios(|_| termios);
    }
}

impl Write for PtyServerHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.update_termios();
        let report = self.client_input.lock().unwrap().write_input(buf)?;
        if let Some(signal) = report.posted_signal
            && let Some(signal_handler) = self.signal_handler
        {
            (signal_handler)(self, signal);
        }
        if report.consumed == 0 && buf.len() > 0 {
            do_sleep(
                // we just need the shared pty without locking
                self.client_output
                    .pty
                    .base()
                    .client_input
                    .sync_for_avail_space(),
            )?;
            return self.write(buf);
        }
        Ok(report.consumed)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Read for PtyServerHandle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.update_termios();
        self.client_output.read(buf)
    }
}

impl Write for PtyClientHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.update_termios();
        self.output.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.update_termios();
        self.output.flush()
    }
}

impl Read for PtyClientHandle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.update_termios();
        let res = self.input.lock().unwrap().read(buf);
        match res {
            Ok(c) => Ok(c),
            Err(e) if e.kind() != ErrorKind::WouldBlock => Err(e),
            _ => {
                if buf.len() == 0 {
                    return Ok(0);
                }
                do_sleep(
                    self.output
                        .writer
                        .pty
                        .base()
                        .client_input
                        .sync_for_pending_data(),
                )?;
                self.read(buf)
            }
        }
    }
}

#[derive(Invariant, BaseType)]
pub struct PtyBase {
    termios_gen: AtomicU64,
    termios: UnsafeCell<libc::termios>,
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

pub const DEFAULT_TERMIOS: libc::termios = libc::termios {
    c_iflag: BRKINT | ISTRIP | ICRNL | IMAXBEL | IXON | IXANY,
    c_oflag: OPOST | ONLCR | XTABS,
    c_cflag: CREAD | CS7 | PARENB | HUPCL,
    c_lflag: ECHO | ICANON | ISIG | IEXTEN | ECHOE | ECHOKE | ECHOCTL,
    c_cc: [
        CINTR,
        CQUIT,
        CERASE,
        CKILL,
        CEOF,
        CTIME,
        CMIN,
        _POSIX_VDISABLE,
        CSTART,
        CSTOP,
        CSUSP,
        CEOL,
        CREPRINT,
        CDISCARD,
        CWERASE,
        CLNEXT,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        CSTATUS,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
    ],
    __c_ispeed: B9600,
    __c_ospeed: B9600,
    c_line: 0,
};

pub const DEFAULT_TERMIOS_RAW: libc::termios = libc::termios {
    c_iflag: ISTRIP | ICRNL,
    c_oflag: ONLCR | XTABS,
    c_cflag: CREAD | CS7 | PARENB | HUPCL,
    c_lflag: 0,
    c_cc: [
        CINTR,
        CQUIT,
        CERASE,
        CKILL,
        CEOF,
        CTIME,
        CMIN,
        _POSIX_VDISABLE,
        CSTART,
        CSTOP,
        CSUSP,
        CEOL,
        CREPRINT,
        CDISCARD,
        CWERASE,
        CLNEXT,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        CSTATUS,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
        _POSIX_VDISABLE,
    ],
    __c_ispeed: B9600,
    __c_ospeed: B9600,
    c_line: 0,
};

impl PtyBase {
    pub fn new(termios: libc::termios) -> Self {
        Self {
            termios_gen: AtomicU64::new(0),
            termios: UnsafeCell::new(termios),
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
    Status,
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
        let echo = self.termios.c_lflag & ECHO != 0;
        let echoe = self.termios.c_lflag & ECHOE != 0 && self.termios.c_lflag & ICANON != 0;
        let echok = self.termios.c_lflag & ECHOK != 0 && self.termios.c_lflag & ICANON != 0;
        let echonl = self.termios.c_lflag & ECHONL != 0 && self.termios.c_lflag & ICANON != 0;

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
                let erase_idx = memchr3(CERASE, CKILL, CWERASE, echobuf);
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
                    match echobuf[idx] {
                        CERASE if echoe => {
                            self.echoer.write_all(&[8, b' ', 8])?;
                            erase_chars(
                                self,
                                (cur_echo_off + idx).saturating_sub(1),
                                cur_echo_off + idx,
                            );
                        }
                        CKILL if echok => {
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
                        }
                        CWERASE if echoe => {
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
                        }
                        b'\n' => {
                            self.echoer.write_all(&[echobuf[idx]])?;
                            self.echobuf_len = 0;
                        }
                        _ => {
                            self.echoer.write_all(&[echobuf[idx]])?;
                        }
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

    pub fn write_input(&mut self, mut buf: &[u8]) -> std::io::Result<WriteReport> {
        let vintr = self.termios.c_cc[VINTR];
        let vquit = self.termios.c_cc[VQUIT];
        let vstatus = self.termios.c_cc[VSTATUS];

        let mut total = 0;
        let mut sig = None;

        while buf.len() > 0 && sig.is_none() {
            let (count, skip) = if let Some(idx) = memchr3(vstatus, vintr, vquit, buf) {
                match buf[idx] {
                    c if c == vintr => sig = Some(PtySignal::Interrupt),
                    c if c == vquit => sig = Some(PtySignal::Quit),
                    c if c == vstatus => sig = Some(PtySignal::Status),
                    _ => unreachable!(),
                }
                (idx, true)
            } else {
                (buf.len(), false)
            };

            let wcount = self.writer.write(&buf[0..count])?;
            let mut ecount = 0;
            while ecount < wcount {
                let mut echobuf = [0; BUF_SZ];
                let remaining = BUF_SZ.min(wcount - ecount);
                echobuf[0..remaining].copy_from_slice(&buf[ecount..wcount]);
                let c = input_map(&self.termios, &mut echobuf[0..remaining]);
                self.maybe_echo(&echobuf[0..c])?;
                ecount += c;
            }

            total += wcount;
            buf = &buf[wcount..];
            if skip && wcount == count {
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
}

impl<W: Write> OutputConverter<W> {
    pub fn new(termios: libc::termios, writer: W) -> Self {
        Self { termios, writer }
    }

    pub fn write_bytes_simple(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.write(buf)
    }

    pub fn write_bytes_processed(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
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
            if let Some(extra) = extra {
                self.writer.write_all(extra)?;
                // Note: we only increment by 1 here because regardless of the extra
                // data we write, it came from 1 byte of the input buffer.
                total += 1;
                buf = &buf[1..];
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
            self.write_bytes_simple(buf)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
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

    pub fn update_termios(&mut self, termios: libc::termios) {
        self.termios = termios;
    }

    fn refill_linebuf(&mut self) -> std::io::Result<()> {
        let linebuf = &mut self.linebuf[self.linebuf_count..];
        let count = self.reader.read(linebuf)?;
        let count = input_map(&self.termios, &mut linebuf[..count]);

        let verase = self.termios.c_cc[VERASE];
        let vwerase = self.termios.c_cc[VWERASE];
        let vkill = self.termios.c_cc[VKILL];

        let count = if let Some(idx) = memchr3(verase, vwerase, vkill, &linebuf[..count]) {
            let idx = idx + self.linebuf_count;

            let rev_idx = match self.linebuf[idx] {
                c if c == verase => {
                    if idx > 0 {
                        if self.linebuf[idx - 1] != b'\n' {
                            idx - 1
                        } else {
                            idx
                        }
                    } else {
                        0
                    }
                }
                c if c == vwerase => memrchr3(b'\n', b' ', b'\t', &self.linebuf[0..idx])
                    .map(|idx| idx + 1)
                    .unwrap_or(0),
                c if c == vkill => memrchr(b'\n', &self.linebuf[0..idx])
                    .map(|idx| idx + 1)
                    .unwrap_or(0),
                _ => panic!("invalid character"),
            };

            self.linebuf.copy_within((idx + 1).., rev_idx);
            self.linebuf_count = self.linebuf_count.saturating_sub((idx - rev_idx).max(1));

            count.saturating_sub((idx - rev_idx).max(1))
        } else {
            count
        };

        self.linebuf_count += count;
        Ok(())
    }

    fn drain_linebuf(&mut self, buf: &mut [u8]) -> (usize, bool) {
        let mut count = buf.len().min(self.linebuf_count);
        let linebuf = &self.linebuf[0..count];

        let mut end = self.linebuf_count == BUF_SZ;
        let veof = self.termios.c_cc[VEOF];

        if let Some(idx) = memchr2(b'\n', veof, linebuf) {
            if linebuf[idx] == b'\n' {
                count = idx + 1;
            } else if linebuf[idx] == veof {
                self.linebuf.copy_within((idx + 1).., idx);
                self.linebuf_count -= 1;
                count = idx;
            }
            end = true;
        }

        if end {
            let linebuf = &self.linebuf[0..count];
            (&mut buf[0..count]).copy_from_slice(linebuf);
            self.linebuf.copy_within(count.., 0);
            self.linebuf_count -= count;
            (count, end)
        } else {
            (0, false)
        }
    }

    pub fn read_canon(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total = 0;
        while buf.len() > 0 {
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
        }
        Ok(total)
    }

    pub fn pending_linebuf(&self) -> usize {
        self.linebuf_count
    }

    pub fn read_raw(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total = 0;
        while buf.len() > 0 {
            let thisread = self.reader.read(buf)?;

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
                b'\n' if nl_to_cr && ignore_cr => {
                    buf.copy_within((idx + 1).., idx);
                    let newend = buf.len() - 1;
                    buf = &mut buf[idx..newend];
                    idx
                }
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

pub mod more_tests {
    use std::io::{Cursor, Seek};

    use libc::{ICANON, ICRNL, IGNCR, INLCR, OCRNL, ONLCR, VEOF, VERASE, VKILL, VWERASE, termios};

    use crate::pty::{InputConverter, OutputConverter};

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
        t.c_cc[VEOF] = 0x4;
        t.c_cc[VERASE] = 0x8;
        t.c_cc[VWERASE] = 0x15;
        t.c_cc[VKILL] = 0x17;
        let mut converter = InputConverter::new(t, &mut input);
        for expected in expected {
            let mut buf = [0u8; 1024];
            let read = converter.read_canon(&mut buf).unwrap();
            assert_eq!(&buf[0..read], *expected);
        }
    }

    pub fn test_raw_input_processing() {
        let input = b"start\ns\rend" as &[u8];
        test_input_processing(0, input, b"start\ns\rend");

        test_input_processing(ICRNL, input, b"start\ns\nend");
        test_input_processing(INLCR, input, b"start\rs\rend");
        test_input_processing(IGNCR, input, b"start\nsend");
        test_input_processing(IGNCR | INLCR, input, b"startsend");
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
        test_input_processing(IGNCR | INLCR, input, b"");
        test_input_processing(IGNCR | ICRNL, input, b"\n");
    }

    pub fn test_canon_input() {
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

        let input = b"test words\x15S\n" as &[u8];
        test_canon(0, input, &[b"test S\n"]);

        let input = b"test\n\x15S\n" as &[u8];
        test_canon(0, input, &[b"test\n", b"S\n"]);

        let input = b"test words\x17S\n" as &[u8];
        test_canon(0, input, &[b"S\n"]);

        let input = b"test\n\x17S\n" as &[u8];
        test_canon(0, input, &[b"test\n", b"S\n"]);
    }

    pub fn test_output() {
        let input = b"start\ns\rend" as &[u8];
        test_output_processing(0, input, b"start\ns\rend");

        test_output_processing(OCRNL, input, b"start\ns\nend");
        test_output_processing(ONLCR, input, b"start\r\ns\rend");
        test_output_processing(ONLCR | OCRNL, input, b"start\r\ns\r\nend");
    }
}
