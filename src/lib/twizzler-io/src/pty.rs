use std::{
    cell::UnsafeCell,
    io::{Read, Write},
    mem::MaybeUninit,
    sync::atomic::{AtomicU64, Ordering},
    thread::current,
};

use libc::{ICANON, ICRNL, IGNCR, INLCR, OCRNL, ONLCR, VEOF, VEOL, VERASE, VINTR, VKILL, VQUIT};
use memchr::{memchr, memchr2};
use twizzler::Invariant;
use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};

struct PtyBuffer<const N: usize> {
    reserve: AtomicU64,
    head: AtomicU64,
    tail: AtomicU64,
    buffer: UnsafeCell<[u8; N]>,
}
unsafe impl<const N: usize> Send for PtyBuffer<N> {}
unsafe impl<const N: usize> Sync for PtyBuffer<N> {}

pub const BUF_SZ: usize = 16;
#[derive(Invariant)]
pub struct PtyBase {
    termios_gen: AtomicU64,
    termios: UnsafeCell<libc::termios>,
    server: PtyBuffer<BUF_SZ>,
    client: PtyBuffer<BUF_SZ>,
}

unsafe impl Send for PtyBase {}
unsafe impl Sync for PtyBase {}

impl PtyBase {
    pub fn new(termios: libc::termios) -> Self {
        Self {
            termios_gen: AtomicU64::new(0),
            termios: UnsafeCell::new(termios),
            server: PtyBuffer::new(),
            client: PtyBuffer::new(),
        }
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

    pub fn read_termios(&self) -> (libc::termios, u64) {
        loop {
            let current_gen = self.termios_gen.load(std::sync::atomic::Ordering::Acquire);
            let val = unsafe { self.termios.get().read() };
            let after_gen = self.termios_gen.load(std::sync::atomic::Ordering::SeqCst);

            if current_gen == after_gen {
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

impl<const N: usize> PtyBuffer<N> {
    fn new() -> Self {
        Self {
            buffer: UnsafeCell::new([0; N]),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            reserve: AtomicU64::new(0),
        }
    }

    fn avail_space(&self) -> usize {
        let tail = self.tail.load(Ordering::SeqCst);
        let resv = self.reserve.load(Ordering::SeqCst);

        (N - 1) - (resv - tail) as usize
    }

    fn pending_bytes(&self) -> usize {
        let head = self.head.load(Ordering::SeqCst);
        let tail = self.tail.load(Ordering::SeqCst);

        (head - tail) as usize
    }

    fn is_empty(&self) -> bool {
        let tail = self.tail.load(Ordering::SeqCst);
        let head = self.head.load(Ordering::SeqCst);

        head == tail
    }

    pub fn read_bytes(&self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut count = 0;
        while buf.len() > 0 {
            let head = self.head.load(Ordering::SeqCst);
            let tail = self.tail.load(Ordering::SeqCst);

            // Empty
            if tail == head {
                return Ok(count);
            }

            assert!(head >= tail);
            let n = std::cmp::min(buf.len(), (head - tail) as usize);
            let n = self.read_from_circle(&mut buf[0..n], tail as usize % N);

            if self
                .tail
                .compare_exchange(tail, tail + n as u64, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                continue;
            }
            buf = &mut buf[n..];
            count += n;
        }
        Ok(count)
    }

    pub fn write_bytes(&self, mut buf: &[u8]) -> std::io::Result<usize> {
        let mut count = 0;
        while buf.len() > 0 {
            let resv = self.reserve.load(Ordering::SeqCst);
            let tail = self.tail.load(Ordering::SeqCst);

            let avail = (N - 1) - (resv - tail) as usize;
            if avail == 0 {
                return Ok(count);
            }

            let n = std::cmp::min(buf.len(), avail);

            // Step 1: reserve space
            if self
                .reserve
                .compare_exchange(resv, resv + n as u64, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                // Someone else reserved space. Try again.
                continue;
            }

            // Step 2: wait until our head catches up to the old reserve. Note that since
            // we succeeded the compare-exchange above, we have to complete this operation
            // for the pty to remain in a consistent state.
            while self.head.load(Ordering::SeqCst) != resv {
                core::hint::spin_loop();
            }

            let n = self.write_to_circle(&buf[0..n], resv as usize % N);

            let old_head = self.head.fetch_add(n as u64, Ordering::SeqCst);
            if old_head != resv {
                tracing::warn!("head incremented unexpectedly ({} != {})", old_head, resv);
            }

            buf = &buf[n..];
            count += n;
        }
        Ok(count)
    }

    fn get_buffer(&self) -> &[u8] {
        let ptr = self.buffer.get();
        unsafe { ptr.as_ref().unwrap() }
    }

    fn get_buffer_mut(&self) -> &mut [u8] {
        let ptr = self.buffer.get();
        unsafe { ptr.as_mut().unwrap() }
    }

    fn read_from_circle(&self, buf: &mut [u8], phase: usize) -> usize {
        let buffer = self.get_buffer();
        let (second, first) = buffer.split_at(phase);
        let first_len = first.len().min(buf.len());
        let second_len = second.len().min(buf.len().saturating_sub(first_len));

        (&mut buf[0..first_len]).copy_from_slice(&first[0..first_len]);
        (&mut buf[first_len..(first_len + second_len)]).copy_from_slice(&second[0..second_len]);
        return first_len + second_len;
    }

    fn write_to_circle(&self, buf: &[u8], phase: usize) -> usize {
        let buffer = self.get_buffer_mut();
        let (second, first) = buffer.split_at_mut(phase);
        let first_len = first.len().min(buf.len());
        let second_len = second.len().min(buf.len().saturating_sub(first_len));

        (&mut first[0..first_len]).copy_from_slice(&buf[0..first_len]);
        (&mut second[0..second_len]).copy_from_slice(&buf[first_len..(first_len + second_len)]);
        return first_len + second_len;
    }

    fn do_wake(&self, ptr: &AtomicU64) {
        let _ = twizzler_abi::syscall::sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(ptr),
                usize::MAX,
            ))],
            None,
        )
        .inspect_err(|e| tracing::error!("failed to wake on termios for pty: {}", e));
    }

    fn do_sleep(&self, ptr: &AtomicU64, val: u64) {
        let _ = twizzler_abi::syscall::sys_thread_sync(
            &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(ptr),
                val,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))],
            None,
        )
        .inspect_err(|e| tracing::error!("failed to wait on termios for pty: {}", e));
    }
}

pub mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize},
    };

    use libc::termios;

    use crate::pty::PtyBase;

    pub fn test_basic() {
        /*
        let t = termios {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: 0,
            c_cc: [0; _],
            __c_ispeed: 0,
            __c_ospeed: 0,
            c_line: 0,
        };
        let pty = PtyBase::new(t);

        let mut buf = [0; 1024];
        assert_eq!(pty.client.read_bytes(&mut buf).unwrap(), 0);

        for i in 0..100 {
            buf.fill(i);

            assert_eq!(pty.client.write_bytes(&buf).unwrap(), 1024);
            assert_eq!(pty.client.read_bytes(&mut buf).unwrap(), 1024);
            assert_eq!(buf, [i; 1024]);
        }

        */
        test_mt();
    }

    pub fn test_mt() {
        let t = termios {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: 0,
            c_cc: [0; _],
            __c_ispeed: 0,
            __c_ospeed: 0,
            c_line: 0,
        };

        const ITER: usize = 100;
        const BS: usize = 1;
        const NR_TH: usize = 8;
        std::thread::scope(|scope| {
            let pty = Arc::new(PtyBase::new(t));

            let counts = Arc::new([const { AtomicUsize::new(0) }; NR_TH]);
            let wcounts = counts.clone();
            let done = Arc::new(AtomicBool::new(false));
            tracing::info!("starting mt pty test");

            let reader = move |done: &AtomicBool, pty: &PtyBase| {
                let do_read = || -> usize {
                    let mut buf = [0; 8];
                    let len = pty.client.read_bytes(&mut buf).unwrap();
                    if len > 0 {
                        tracing::info!("rr: {} {}", len, buf[0]);
                    }
                    for b in &buf[0..len] {
                        let idx = *b as usize;
                        tracing::info!("      => {}", idx);
                        wcounts[idx].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    len
                };
                while !done.load(std::sync::atomic::Ordering::SeqCst) {
                    do_read();
                }
                while do_read() > 0 {}
            };

            let writer = |pty: &PtyBase, c: u8| {
                for i in 0..ITER {
                    let buf = [c; BS];
                    tracing::info!("ww: {} {}", c, i);
                    let mut len = pty.client.write_bytes(&buf).unwrap();
                    while len == 0 {
                        tracing::info!("{} had to retry", c);
                        len = pty.client.write_bytes(&buf).unwrap();
                    }
                }
            };

            let wpty = pty.clone();
            let wdone = done.clone();
            let rd = scope.spawn(move || reader(&wdone, &wpty));
            let ws = (0..NR_TH)
                .map(|i| {
                    let pty = pty.clone();
                    scope.spawn(move || writer(&pty, i as u8))
                })
                .collect::<Vec<_>>();

            for t in ws {
                t.join().unwrap();
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            rd.join().unwrap();

            let expected = ITER * BS;
            for count in (&*counts).iter().enumerate() {
                let nr = count.1.load(std::sync::atomic::Ordering::SeqCst);
                if nr != expected {
                    tracing::warn!("{}: found wrong count: {} {}", count.0, nr, expected);
                }
            }
        });
        tracing::info!("finished mt pty test");
    }
}

pub struct OutputConverter<'a, W: Write> {
    termios: libc::termios,
    writer: &'a mut W,
}

impl<'a, W: Write> OutputConverter<'a, W> {
    pub fn new(termios: libc::termios, writer: &'a mut W) -> Self {
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

pub struct InputConverter<'a, R: Read> {
    termios: libc::termios,
    linebuf: [u8; BUF_SZ],
    linebuf_count: usize,
    reader: &'a mut R,
}

impl<'a, R: Read> InputConverter<'a, R> {
    pub fn new(termios: libc::termios, reader: &'a mut R) -> Self {
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
        if self.linebuf_count == 0 {
            let count = self.reader.read(&mut self.linebuf)?;
            let count = Self::input_map(&self.termios, &mut self.linebuf[0..count]);
            self.linebuf_count = count;
        }
        Ok(())
    }

    fn drain_linebuf(&mut self, buf: &mut [u8]) -> (usize, bool) {
        let mut count = buf.len().min(self.linebuf_count);
        let linebuf = &self.linebuf[0..count];

        let veof = self.termios.c_cc[VEOF];
        //let vstatus = self.termios.c_cc[VSTATUS];
        let vquit = self.termios.c_cc[VQUIT];
        let vintr = self.termios.c_cc[VINTR];

        let mut end = false;
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

        for needle in [vintr, vquit] {
            let linebuf = &self.linebuf[0..count];
            if let Some(idx) = memchr(needle, linebuf) {
                self.linebuf.copy_within((idx + 1).., idx);
                self.linebuf_count -= 1;
                count -= 1;
            }
        }

        let linebuf = &self.linebuf[0..count];
        (&mut buf[0..count]).copy_from_slice(linebuf);
        self.linebuf.copy_within(count.., 0);
        self.linebuf_count -= count;
        (count, end)
    }

    pub fn read_canon(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total = 0;
        while buf.len() > 0 {
            self.refill_linebuf()?;
            if self.linebuf_count == 0 {
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

    pub fn read_raw(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total = 0;
        while buf.len() > 0 {
            let thisread = self.reader.read(buf)?;

            if thisread == 0 {
                return Ok(total);
            }

            // this might squash characters
            let thisread = Self::input_map(&self.termios, &mut buf[0..thisread]);

            total += thisread;
            buf = &mut buf[thisread..];
        }
        Ok(total)
    }
}

impl<R: Read> Read for InputConverter<'_, R> {
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

    use libc::{ICANON, ICRNL, IGNCR, INLCR, OCRNL, ONLCR, VEOF, termios};

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
    }

    pub fn test_output() {
        let input = b"start\ns\rend" as &[u8];
        test_output_processing(0, input, b"start\ns\rend");

        test_output_processing(OCRNL, input, b"start\ns\nend");
        test_output_processing(ONLCR, input, b"start\r\ns\rend");
        test_output_processing(ONLCR | OCRNL, input, b"start\r\ns\r\nend");
    }
}
