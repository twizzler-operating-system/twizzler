use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering},
};

use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};

pub struct VolatileBuffer<const N: usize> {
    reserve: AtomicU64,
    head: AtomicU64,
    tail: AtomicU64,
    buffer: UnsafeCell<[u8; N]>,
}
unsafe impl<const N: usize> Send for VolatileBuffer<N> {}
unsafe impl<const N: usize> Sync for VolatileBuffer<N> {}

impl<const N: usize> VolatileBuffer<N> {
    pub fn new() -> Self {
        Self {
            buffer: UnsafeCell::new([0; N]),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            reserve: AtomicU64::new(0),
        }
    }

    pub fn avail_space(&self) -> usize {
        let tail = self.tail.load(Ordering::SeqCst);
        let resv = self.reserve.load(Ordering::SeqCst);

        (N - 1) - (resv - tail) as usize
    }

    pub fn pending_bytes(&self) -> usize {
        let head = self.head.load(Ordering::SeqCst);
        let tail = self.tail.load(Ordering::SeqCst);

        (head - tail) as usize
    }

    pub fn is_empty(&self) -> bool {
        let tail = self.tail.load(Ordering::SeqCst);
        let head = self.head.load(Ordering::SeqCst);

        head == tail
    }

    pub fn sync_for_pending_data(&self) -> ThreadSyncSleep {
        let head = self.head.load(Ordering::SeqCst);
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.head),
            head,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    pub fn sync_for_avail_space(&self) -> ThreadSyncSleep {
        let tail = self.tail.load(Ordering::SeqCst);
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.tail),
            tail,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
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
            self.do_wake(&self.tail);
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
            self.do_wake(&self.head);

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
        .inspect_err(|e| tracing::error!("failed to wake on volatile buffer: {}", e));
    }
}

#[cfg(test)]
pub mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize},
    };

    use crate::buffer::VolatileBuffer;

    #[test]
    pub fn test_basic() {
        let vb = VolatileBuffer::<2048>::new();

        let mut buf = [0; 1024];
        assert_eq!(vb.read_bytes(&mut buf).unwrap(), 0);

        for i in 0..100 {
            buf.fill(i);

            assert_eq!(vb.write_bytes(&buf).unwrap(), 1024);
            assert_eq!(vb.read_bytes(&mut buf).unwrap(), 1024);
            assert_eq!(buf, [i; 1024]);
        }
    }

    #[test]
    pub fn test_mt() {
        const ITER: usize = 100;
        const BS: usize = 1;
        const NR_TH: usize = 8;
        std::thread::scope(|scope| {
            let vb = Arc::new(VolatileBuffer::<2048>::new());

            let counts = Arc::new([const { AtomicUsize::new(0) }; NR_TH]);
            let wcounts = counts.clone();
            let done = Arc::new(AtomicBool::new(false));
            tracing::info!("starting mt pty test");

            let reader = move |done: &AtomicBool, pty: &VolatileBuffer<_>| {
                let do_read = || -> usize {
                    let mut buf = [0; 8];
                    let len = pty.read_bytes(&mut buf).unwrap();
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

            let writer = |pty: &VolatileBuffer<_>, c: u8| {
                for i in 0..ITER {
                    let buf = [c; BS];
                    tracing::info!("ww: {} {}", c, i);
                    let mut len = pty.write_bytes(&buf).unwrap();
                    while len == 0 {
                        tracing::info!("{} had to retry", c);
                        len = pty.write_bytes(&buf).unwrap();
                    }
                }
            };

            let wpty = vb.clone();
            let wdone = done.clone();
            let rd = scope.spawn(move || reader(&wdone, &*wpty));
            let ws = (0..NR_TH)
                .map(|i| {
                    let pty = vb.clone();
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
