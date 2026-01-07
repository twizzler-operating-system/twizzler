use std::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU64, Ordering},
    thread::current,
};

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

pub const BUF_SZ: usize = 8192;
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

            let old_head = self.head.fetch_add(n as u64, Ordering::Release);
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
        let (first, second) = buffer.split_at(phase);
        let first_len = first.len().min(buf.len());
        let second_len = second.len().min(buf.len().saturating_sub(first_len));

        (&mut buf[0..first_len]).copy_from_slice(&first[0..first_len]);
        (&mut buf[first_len..(first_len + second_len)]).copy_from_slice(&second[0..second_len]);
        return first_len + second_len;
    }

    fn write_to_circle(&self, buf: &[u8], phase: usize) -> usize {
        let buffer = self.get_buffer_mut();
        let (first, second) = buffer.split_at_mut(phase);
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
        const NR_TH: usize = 2;
        std::thread::scope(|scope| {
            let pty = Arc::new(PtyBase::new(t));

            let counts = Arc::new([const { AtomicUsize::new(0) }; NR_TH]);
            let wcounts = counts.clone();
            let done = Arc::new(AtomicBool::new(false));
            tracing::info!("starting mt pty test");

            let reader = move |done: &AtomicBool, pty: &PtyBase| {
                while !done.load(std::sync::atomic::Ordering::SeqCst) {
                    let mut buf = [0; 8];
                    let len = pty.client.read_bytes(&mut buf).unwrap();
                    tracing::info!("rr: {} {}", len, buf[0]);
                    for b in &buf[0..len] {
                        let idx = *b as usize;
                        tracing::info!("      => {}", idx);
                        wcounts[idx].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            };

            let writer = |pty: &PtyBase, c: u8| {
                for i in 0..100 {
                    let mut buf = [c; BS];
                    tracing::info!("ww: {} {}", c, i);
                    let mut len = pty.client.write_bytes(&mut buf).unwrap();
                    while len == 0 {
                        len = pty.client.write_bytes(&mut buf).unwrap();
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
