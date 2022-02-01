#![feature(termination_trait_lib)]
#![feature(test)]

use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};
#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub struct QueueEntry<T> {
    cmd_slot: u32,
    info: u32,
    data: T,
}

impl<T> QueueEntry<T> {
    #[inline]
    fn get_cmd_slot(&self) -> u32 {
        unsafe { core::mem::transmute::<&u32, &AtomicU32>(&self.cmd_slot).load(Ordering::SeqCst) }
    }

    #[inline]
    fn set_cmd_slot(&self, v: u32) {
        unsafe {
            core::mem::transmute::<&u32, &AtomicU32>(&self.cmd_slot).store(v, Ordering::SeqCst);
        }
    }

    #[inline]
    pub fn item(self) -> T {
        self.data
    }

    #[inline]
    pub fn info(&self) -> u32 {
        self.info
    }

    pub fn new(info: u32, item: T) -> Self {
        Self {
            cmd_slot: 0,
            info,
            data: item,
        }
    }
}

#[repr(C)]
pub struct RawQueueHdr {
    l2len: usize,
    stride: usize,
    head: AtomicU32,
    waiters: AtomicU32,
    bell: AtomicU64,
    tail: AtomicU64,
}

impl RawQueueHdr {
    pub fn new(l2len: usize, stride: usize) -> Self {
        Self {
            l2len,
            stride,
            head: AtomicU32::new(0),
            waiters: AtomicU32::new(0),
            bell: AtomicU64::new(0),
            tail: AtomicU64::new(0),
        }
    }

    #[inline]
    fn len(&self) -> usize {
        1 << self.l2len
    }

    #[inline]
    fn is_full(&self, h: u32, t: u64) -> bool {
        (h & 0x7fffffff) as u64 - (t & 0x7fffffff) >= self.len() as u64
    }

    #[inline]
    fn is_empty(&self, bell: u64, tail: u64) -> bool {
        (bell & 0x7fffffff) == (tail & 0x7fffffff)
    }

    #[inline]
    fn is_turn<T>(&self, t: u64, item: *const QueueEntry<T>) -> bool {
        let turn = (t / (self.len() as u64)) % 2;
        let val = unsafe { &*item }.get_cmd_slot() >> 31;
        (val == 0) == (turn == 1)
    }

    #[inline]
    fn consumer_waiting(&self) -> bool {
        (self.tail.load(Ordering::SeqCst) & (1 << 31)) != 0
    }

    #[inline]
    fn submitter_waiting(&self) -> bool {
        self.waiters.load(Ordering::SeqCst) > 0
    }

    #[inline]
    fn consumer_set_waiting(&self, waiting: bool) {
        if waiting {
            self.tail.fetch_or(1 << 31, Ordering::SeqCst);
        } else {
            self.tail.fetch_and(!(1 << 31), Ordering::SeqCst);
        }
    }

    #[inline]
    fn inc_submit_waiting(&self) {
        self.waiters.fetch_add(1, Ordering::SeqCst);
    }

    #[inline]
    fn dec_submit_waiting(&self) {
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }

    fn reserve_slot<W: Fn(&AtomicU64, u64)>(
        &self,
        flags: SubmissionFlags,
        wait: W,
    ) -> Result<u32, SubmissionError> {
        let h = self.head.fetch_add(1, Ordering::SeqCst);
        let mut waiter = false;
        let mut attempts = 1000;
        loop {
            let t = self.tail.load(Ordering::SeqCst);
            if !self.is_full(h, t) {
                break;
            }

            if flags.contains(SubmissionFlags::NON_BLOCK) {
                return Err(SubmissionError::WouldBlock);
            }

            if attempts != 0 {
                attempts -= 1;
                core::hint::spin_loop();
                continue;
            }

            if !waiter {
                waiter = true;
                self.inc_submit_waiting();
            }

            let t = self.tail.load(Ordering::SeqCst);
            if self.is_full(h, t) {
                wait(&self.tail, t);
            }
        }

        if waiter {
            self.dec_submit_waiting();
        }

        Ok(h & 0x7fffffff)
    }

    #[inline]
    fn get_turn(&self, h: u32) -> bool {
        (h / self.len() as u32) % 2 == 0
    }

    #[inline]
    fn ring<R: Fn(&AtomicU64)>(&self, ring: R) {
        self.bell.fetch_add(1, Ordering::SeqCst);
        if self.consumer_waiting() {
            ring(&self.bell)
        }
    }

    fn get_next_ready<W: Fn(&AtomicU64, u64), T>(
        &self,
        wait: W,
        flags: ReceiveFlags,
        raw_buf: *const QueueEntry<T>,
    ) -> Result<u64, ReceiveError> {
        let mut attempts = 1000;
        let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
        loop {
            let b = self.bell.load(Ordering::SeqCst);
            let item = unsafe { raw_buf.add((t as usize) % self.len()) };

            if !self.is_empty(b, t) && self.is_turn(t, item) {
                break;
            }

            if flags.contains(ReceiveFlags::NON_BLOCK) {
                return Err(ReceiveError::WouldBlock);
            }

            if attempts != 0 {
                attempts -= 1;
                core::hint::spin_loop();
                continue;
            }

            self.consumer_set_waiting(true);
            let b = self.bell.load(Ordering::SeqCst);
            if self.is_empty(b, t) || !self.is_turn(t, item) {
                wait(&self.bell, b);
            }
        }

        if attempts == 0 {
            self.consumer_set_waiting(false);
        }
        Ok(t)
    }

    #[inline]
    fn advance_tail<R: Fn(&AtomicU64)>(&self, ring: R) {
        let t = self.tail.load(Ordering::SeqCst);
        self.tail.store((t + 1) & 0x7fffffff, Ordering::SeqCst);
        if self.submitter_waiting() {
            ring(&self.tail);
        }
    }
}

pub struct RawQueue<'a, T> {
    hdr: &'a RawQueueHdr,
    buf: UnsafeCell<*mut QueueEntry<T>>,
}

bitflags::bitflags! {
    pub struct SubmissionFlags: u32 {
        const NON_BLOCK = 1;
    }

    pub struct ReceiveFlags: u32 {
        const NON_BLOCK = 1;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubmissionError {
    Unknown,
    WouldBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReceiveError {
    Unknown,
    WouldBlock,
}

impl<'a, T: Copy> RawQueue<'a, T> {
    pub fn new(hdr: &'a RawQueueHdr, buf: *mut QueueEntry<T>) -> Self {
        Self {
            hdr,
            buf: UnsafeCell::new(buf),
        }
    }

    #[inline]
    fn get_buf(&self, off: usize) -> &mut QueueEntry<T> {
        unsafe {
            (*self.buf.get())
                .add(off % self.hdr.len())
                .as_mut()
                .unwrap()
        }
    }

    pub fn submit<W: Fn(&AtomicU64, u64), R: Fn(&AtomicU64)>(
        &self,
        item: QueueEntry<T>,
        wait: W,
        ring: R,
        flags: SubmissionFlags,
    ) -> Result<(), SubmissionError> {
        let h = self.hdr.reserve_slot(flags, wait)?;
        let buf_item = self.get_buf(h as usize);
        *buf_item = item;
        let turn = self.hdr.get_turn(h);
        buf_item.set_cmd_slot(h | if turn { 1u32 << 31 } else { 0 });

        self.hdr.ring(ring);
        Ok(())
    }

    pub fn receive<W: Fn(&AtomicU64, u64), R: Fn(&AtomicU64)>(
        &self,
        wait: W,
        ring: R,
        flags: ReceiveFlags,
    ) -> Result<QueueEntry<T>, ReceiveError> {
        let t = self
            .hdr
            .get_next_ready(wait, flags, unsafe { *self.buf.get() })?;
        let buf_item = self.get_buf(t as usize);
        let item = *buf_item;
        self.hdr.advance_tail(ring);
        Ok(item)
    }
}

unsafe impl<'a, T: Send> Send for RawQueue<'a, T> {}
unsafe impl<'a, T: Send> Sync for RawQueue<'a, T> {}

#[cfg(test)]
mod tests {
    #![allow(soft_unstable)]
    use std::process::Termination;
    use std::sync::atomic::{AtomicU64, Ordering};

    use syscalls::SyscallArgs;

    use crate::ReceiveError;
    use crate::SubmissionError;
    use crate::{QueueEntry, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags};

    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }

    fn wait(x: &AtomicU64, v: u64) {
        // println!("wait");
        unsafe {
            syscalls::syscall(
                syscalls::SYS_futex,
                &SyscallArgs::new(x as *const AtomicU64 as usize, 0, v as usize, 0, 0, 0),
            );
        }
        /*
        while x.load(Ordering::SeqCst) == v {
            core::hint::spin_loop();
        }
        */
    }

    fn wake(x: &AtomicU64) {
        //   println!("wake");
        unsafe {
            syscalls::syscall(
                syscalls::SYS_futex,
                &SyscallArgs::new(x as *const AtomicU64 as usize, 1, !0, 0, 0, 0),
            );
        }
    }

    #[test]
    fn it_transmits() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
        let mut q = RawQueue::new(&qh, buffer.as_mut_ptr());

        for i in 0..100 {
            let res = q.submit(
                QueueEntry::new(i as u32, i * 10),
                wait,
                wake,
                SubmissionFlags::empty(),
            );
            assert_eq!(res, Ok(()));
            let res = q.receive(wait, wake, ReceiveFlags::empty());
            assert!(res.is_ok());
            assert_eq!(res.unwrap().info(), i as u32);
            assert_eq!(res.unwrap().item(), i * 10);
        }
    }

    #[test]
    fn it_fills() {
        let qh = RawQueueHdr::new(2, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 2];
        let mut q = RawQueue::new(&qh, buffer.as_mut_ptr());

        let res = q.submit(QueueEntry::new(1, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.submit(QueueEntry::new(2, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.submit(QueueEntry::new(3, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.submit(QueueEntry::new(4, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.submit(
            QueueEntry::new(1, 7),
            wait,
            wake,
            SubmissionFlags::NON_BLOCK,
        );
        assert_eq!(res, Err(SubmissionError::WouldBlock));
    }

    #[test]
    fn it_nonblock_receives() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
        let mut q = RawQueue::new(&qh, buffer.as_mut_ptr());

        let res = q.submit(QueueEntry::new(1, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.receive(wait, wake, ReceiveFlags::empty());
        assert!(res.is_ok());
        assert_eq!(res.unwrap().info(), 1);
        assert_eq!(res.unwrap().item(), 7);
        let res = q.receive(wait, wake, ReceiveFlags::NON_BLOCK);
        assert_eq!(res.unwrap_err(), ReceiveError::WouldBlock);
    }

    use std::thread::Thread;
    extern crate crossbeam;
    extern crate test;
    #[bench]
    fn two_threads(b: &mut test::Bencher) -> impl Termination {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
        let mut q = RawQueue::new(
            unsafe { std::mem::transmute::<&RawQueueHdr, &'static RawQueueHdr>(&qh) },
            buffer.as_mut_ptr(),
        );

        let count = AtomicU64::new(0);
        let x = crossbeam::scope(|s| {
            let j = s.spawn(|_| loop {
                let res = q.receive(wait, wake, ReceiveFlags::empty());
                assert!(res.is_ok());
                if res.unwrap().info() == 2 {
                    break;
                }
                count.fetch_add(1, Ordering::SeqCst);
            });

            b.iter(|| {
                let res = q.submit(QueueEntry::new(1, 2), wait, wake, SubmissionFlags::empty());
                assert_eq!(res, Ok(()));
            });
            let res = q.submit(QueueEntry::new(2, 2), wait, wake, SubmissionFlags::empty());
            assert_eq!(res, Ok(()));
        });

        x.unwrap();
    }
}
