//! A raw queue interface for Twizzler, making no assumptions about where the underlying headers and
//! circular buffers are located. This means you probably don't want to use this --- instead, I
//! suggest you use the wrapped version of this library, twizzler-queue, since that actually
//! interacts with the object system.
//!
//! This library exists to provide an underlying implementation of the concurrent data structure for
//! each individual raw queue so that this complex code can be reused in both userspace and the
//! kernel.
//!
//! The basic design of a raw queue is two parts:
//!
//!   1. A header, which contains things like head pointers, tail pointers, etc.
//!   2. A buffer, which contains the items that are enqueued.
//!
//! The queue is an MPSC lock-free blocking data structure. Any thread may submit to a queue, but
//! only one thread may receive on that queue at a time. The queue is implemented with a head
//! pointer, a tail pointer, a doorbell, and a waiters counter. Additionally, the queue is
//! maintained in terms of "turns", that indicate which "go around" of the queue we are on (mod 2).
//!
//! # Let's look at an insert
//! Here's what the queue looks like to start with. The 0_ indicates that it's empty, and turn is
//! set to 0.
//! ```text
//!  b
//!  t
//!  h
//! [0_, 0_, 0_]
//! ```
//! When inserting, the thread first reserves space:
//! ```text
//!  b
//!  t
//!      h
//! [0_, 0_, 0_]
//! ```
//! Then it fills out the data:
//! ```text
//!  b
//!  t
//!      h
//! [0X, 0_, 0_]
//! ```
//! Then it toggles the turn bit:
//! ```text
//!  b
//!  t
//!      h
//! [1X, 0_, 0_]
//! ```
//! Next, it bumps the doorbell (and maybe wakes up a waiting consumer):
//! ```text
//!      b
//!  t
//!      h
//! [1X, 0_, 0_]
//! ```
//!
//! Now, let's say the consumer comes along and dequeues. First, it checks if it's empty by
//! comparing tail and bell, and finds it's not empty. Then it checks if it's the correct turn. This
//! turn is 1, so yes. Next, it remove the data from the queue:
//! ```text
//!      b
//!  t
//!      h
//! [1_, 0_, 0_]
//! ```
//! And then finally it increments the tail counter:
//! ```text
//!      b
//!      t
//!      h
//! [1_, 0_, 0_]
//! ```

#![cfg_attr(test, feature(test))]
#![cfg_attr(not(any(feature = "std", test)), no_std)]

use core::{
    cell::UnsafeCell,
    fmt::Display,
    marker::PhantomData,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use twizzler_abi::marker::BaseType;
#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
/// A queue entry. All queues must be formed of these, as the queue algorithm uses data inside this
/// struct as part of its operation. The cmd_slot is used internally to track turn, and the info is
/// used by the full queue structure to manage completion. The data T is user data passed around the
/// queue.
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
    /// Get the data item of a QueueEntry.
    pub fn item(self) -> T {
        self.data
    }

    #[inline]
    /// Get the info tag of a QueueEntry.
    pub fn info(&self) -> u32 {
        self.info
    }

    /// Construct a new QueueEntry. The `info` tag should be used to inform completion events in the
    /// full queue.
    pub fn new(info: u32, item: T) -> Self {
        Self {
            cmd_slot: 0,
            info,
            data: item,
        }
    }
}

/// The base info structure stored in a Twizzler queue object. Used to open Twizzler queue objects
/// and create a [Queue].
#[repr(C)]
pub struct QueueBase<S, C> {
    pub sub_hdr: usize,
    pub com_hdr: usize,
    pub sub_buf: usize,
    pub com_buf: usize,
    _pd: PhantomData<(S, C)>,
}

impl<S, C> BaseType for QueueBase<S, C> {
    fn init<T>(_t: T) -> Self {
        todo!()
    }

    fn tags() -> &'static [(
        twizzler_abi::marker::BaseVersion,
        twizzler_abi::marker::BaseTag,
    )] {
        todo!()
    }
}

#[repr(C)]
/// A raw queue header. This contains all the necessary counters and info to run the queue
/// algorithm.
pub struct RawQueueHdr {
    l2len: usize,
    stride: usize,
    head: AtomicU32,
    waiters: AtomicU32,
    bell: AtomicU64,
    tail: AtomicU64,
}

impl RawQueueHdr {
    /// Construct a new raw queue header.
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

    #[inline]
    fn reserve_slot<W: Fn(&AtomicU64, u64)>(
        &self,
        flags: SubmissionFlags,
        wait: W,
    ) -> Result<u32, QueueError> {
        let h = self.head.fetch_add(1, Ordering::SeqCst);
        let mut waiter = false;
        let mut attempts = 1000;
        loop {
            let t = self.tail.load(Ordering::SeqCst);
            if !self.is_full(h, t) {
                break;
            }

            if flags.contains(SubmissionFlags::NON_BLOCK) {
                return Err(QueueError::WouldBlock);
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

    #[inline]
    fn get_next_ready<W: Fn(&AtomicU64, u64), T>(
        &self,
        wait: W,
        flags: ReceiveFlags,
        raw_buf: *const QueueEntry<T>,
    ) -> Result<u64, QueueError> {
        let mut attempts = 1000;
        let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
        loop {
            let b = self.bell.load(Ordering::SeqCst);
            let item = unsafe { raw_buf.add((t as usize) & (self.len() - 1)) };

            if !self.is_empty(b, t) && self.is_turn(t, item) {
                break;
            }

            if flags.contains(ReceiveFlags::NON_BLOCK) {
                return Err(QueueError::WouldBlock);
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

    fn setup_rec_sleep_simple(&self) -> (&AtomicU64, u64) {
        // TODO: an interface that undoes this.
        self.consumer_set_waiting(true);
        let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
        (&self.bell, t)
    }

    fn setup_send_sleep_simple(&self) -> (&AtomicU64, u64) {
        // TODO: an interface that undoes this.
        self.submitter_waiting();
        let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
        let h = self.head.load(Ordering::SeqCst) & 0x7fffffff;
        if self.is_full(h, t) {
            (&self.tail, t)
        } else {
            (&self.tail, u64::MAX)
        }
    }

    fn setup_rec_sleep<'a, T>(
        &'a self,
        sleep: bool,
        raw_buf: *const QueueEntry<T>,
        waiter: &mut (Option<&'a AtomicU64>, u64),
    ) -> Result<u64, QueueError> {
        let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
        let b = self.bell.load(Ordering::SeqCst);
        let item = unsafe { raw_buf.add((t as usize) & (self.len() - 1)) };
        *waiter = (Some(&self.bell), b);
        if self.is_empty(b, t) || !self.is_turn(t, item) {
            if sleep {
                self.consumer_set_waiting(true);
                let b = self.bell.load(Ordering::SeqCst);
                *waiter = (Some(&self.bell), b);
                if !self.is_empty(b, t) && self.is_turn(t, item) {
                    return Ok(t);
                }
            }
            Err(QueueError::WouldBlock)
        } else {
            Ok(t)
        }
    }

    #[inline]
    fn advance_tail<R: Fn(&AtomicU64)>(&self, ring: R) {
        let t = self.tail.load(Ordering::SeqCst);
        self.tail.store((t + 1) & 0x7fffffff, Ordering::SeqCst);
        if self.submitter_waiting() {
            ring(&self.tail);
        }
    }

    #[inline]
    fn advance_tail_setup<'a>(&'a self, ringer: &mut Option<&'a AtomicU64>) {
        let t = self.tail.load(Ordering::SeqCst);
        self.tail.store((t + 1) & 0x7fffffff, Ordering::SeqCst);
        if self.submitter_waiting() {
            *ringer = Some(&self.tail);
        }
    }
}

/// A raw queue, comprising of a header to track the algorithm and a buffer to hold queue entries.
pub struct RawQueue<T> {
    hdr: *const RawQueueHdr,
    buf: UnsafeCell<*mut QueueEntry<T>>,
}

bitflags::bitflags! {
    /// Flags to control how queue submission works.
    pub struct SubmissionFlags: u32 {
        /// If the request would block, return Err([SubmissionError::WouldBlock]) instead.
        const NON_BLOCK = 1;
    }

    /// Flags to control how queue receive works.
    pub struct ReceiveFlags: u32 {
        /// If the request would block, return Err([ReceiveError::WouldBlock]) instead.
        const NON_BLOCK = 1;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Possible errors for submitting to a queue.
pub enum QueueError {
    /// An unknown error.
    Unknown,
    /// The operation would have blocked, and non-blocking operation was specified.
    WouldBlock,
}

impl Display for QueueError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown"),
            Self::WouldBlock => write!(f, "would block"),
        }
    }
}

impl core::error::Error for QueueError {}

#[cfg(feature = "std")]
impl From<QueueError> for std::io::Error {
    fn from(err: QueueError) -> Self {
        match err {
            QueueError::WouldBlock => std::io::Error::from(std::io::ErrorKind::WouldBlock),
            _ => std::io::Error::from(std::io::ErrorKind::Other),
        }
    }
}

impl<T: Copy> RawQueue<T> {
    /// Construct a new raw queue out of a header reference and a buffer pointer.
    /// # Safety
    /// The caller must ensure that hdr and buf point to valid objects, and that the lifetime of the
    /// RawQueue is exceeded by the objects pointed to.
    pub unsafe fn new(hdr: *const RawQueueHdr, buf: *mut QueueEntry<T>) -> Self {
        Self {
            hdr,
            buf: UnsafeCell::new(buf),
        }
    }

    #[inline]
    fn hdr(&self) -> &RawQueueHdr {
        unsafe { &*self.hdr }
    }

    // This is a bit unsafe, but it's because we're managing concurrency ourselves.
    #[allow(clippy::mut_from_ref)]
    #[inline]
    fn get_buf(&self, off: usize) -> &mut QueueEntry<T> {
        unsafe {
            (*self.buf.get())
                .add(off & (self.hdr().len() - 1))
                .as_mut()
                .unwrap()
        }
    }

    /// Submit a data item of type T, wrapped in a QueueEntry, to the queue. The two callbacks,
    /// wait, and ring, are for implementing a rudimentary condvar, wherein if the queue needs to
    /// block, we'll call wait(x, y), where we are supposed to wait until *x != y. Once we are done
    /// inserting, if we need to wake up a consumer, we will call ring, which should wake up anyone
    /// waiting on that word of memory.
    pub fn submit<W: Fn(&AtomicU64, u64), R: Fn(&AtomicU64)>(
        &self,
        item: QueueEntry<T>,
        wait: W,
        ring: R,
        flags: SubmissionFlags,
    ) -> Result<(), QueueError> {
        let h = self.hdr().reserve_slot(flags, wait)?;
        let buf_item = self.get_buf(h as usize);
        *buf_item = item;
        let turn = self.hdr().get_turn(h);
        buf_item.set_cmd_slot(h | if turn { 1u32 << 31 } else { 0 });

        self.hdr().ring(ring);
        Ok(())
    }

    /// Receive data from the queue, returning either that data or an error. The wait and ring
    /// callbacks work similar to [RawQueue::submit].
    pub fn receive<W: Fn(&AtomicU64, u64), R: Fn(&AtomicU64)>(
        &self,
        wait: W,
        ring: R,
        flags: ReceiveFlags,
    ) -> Result<QueueEntry<T>, QueueError> {
        let t = self
            .hdr()
            .get_next_ready(wait, flags, unsafe { *self.buf.get() })?;
        let buf_item = self.get_buf(t as usize);
        let item = *buf_item;
        self.hdr().advance_tail(ring);
        Ok(item)
    }

    pub fn setup_sleep<'a>(
        &'a self,
        sleep: bool,
        output: &mut Option<QueueEntry<T>>,
        waiter: &mut (Option<&'a AtomicU64>, u64),
        ringer: &mut Option<&'a AtomicU64>,
    ) -> Result<(), QueueError> {
        let t = self
            .hdr()
            .setup_rec_sleep(sleep, unsafe { *self.buf.get() }, waiter)?;
        let buf_item = self.get_buf(t as usize);
        let item = *buf_item;
        *output = Some(item);
        self.hdr().advance_tail_setup(ringer);
        Ok(())
    }

    #[inline]
    pub fn setup_sleep_simple(&self) -> (&AtomicU64, u64) {
        self.hdr().setup_rec_sleep_simple()
    }

    #[inline]
    pub fn setup_send_sleep_simple(&self) -> (&AtomicU64, u64) {
        self.hdr().setup_send_sleep_simple()
    }
}

unsafe impl<T: Send> Send for RawQueue<T> {}
unsafe impl<T: Send> Sync for RawQueue<T> {}

#[cfg(any(feature = "std", test))]
/// Wait for receiving on multiple raw queues. If any of the passed raw queues can return data, they
/// will do so by writing it into the output array at the same index that they are in the `queues`
/// variable. The queues and output arrays must be the same length. If no data is available in any
/// queues, then the function will call back on multi_wait, which it expects to wait until **any**
/// of the pairs (&x, y) meet the condition that *x != y. Before returning any data, the function
/// will callback on multi_ring, to inform multiple queues that data was taken from them. It expects
/// the multi_ring function to wake up any waiting threads on the supplied words of memory.
///
/// Note that both call backs specify the pointers as Option. In the case that an entry is None,
/// there was no requested wait or wake operation for that queue, and that entry should be ignored.
///
/// If flags specifies [ReceiveFlags::NON_BLOCK], then if no data is available, the function returns
/// immediately with Err([QueueError::WouldBlock]).
///
/// # Rationale
/// This function is here to implement poll or select like functionality, wherein a given thread or
/// program wants to wait on multiple incoming request channels and handle them itself, thus cutting
/// down on the number of threads required. The maximum number of queues to use here is a trade-off
/// --- more means fewer threads, but since this function is linear in the number of queues, each
/// thread could take longer to service requests.
///
/// The complexity of the multi_wait and multi_ring callbacks is present to avoid calling into the
/// kernel often for high-contention queues.
pub fn multi_receive<T: Copy, W: Fn(&[(Option<&AtomicU64>, u64)]), R: Fn(&[Option<&AtomicU64>])>(
    queues: &[&RawQueue<T>],
    output: &mut [Option<QueueEntry<T>>],
    multi_wait: W,
    multi_ring: R,
    flags: ReceiveFlags,
) -> Result<usize, QueueError> {
    if output.len() != queues.len() {
        return Err(QueueError::Unknown);
    }
    /* TODO (opt): avoid this allocation until we have to sleep */
    let mut waiters = Vec::new();
    waiters.resize(queues.len(), Default::default());
    let mut ringers = Vec::new();
    ringers.resize(queues.len(), None);
    let mut attempts = 100;
    loop {
        let mut count = 0;
        for (i, q) in queues.iter().enumerate() {
            let res = q.setup_sleep(
                attempts == 0,
                &mut output[i],
                &mut waiters[i],
                &mut ringers[i],
            );
            if res == Ok(()) {
                count += 1;
            }
        }
        if count > 0 {
            multi_ring(&ringers);
            return Ok(count);
        }
        if flags.contains(ReceiveFlags::NON_BLOCK) {
            return Err(QueueError::WouldBlock);
        }
        if attempts > 0 {
            attempts -= 1;
        } else {
            multi_wait(&waiters);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(soft_unstable)]
    use std::sync::atomic::{AtomicU64, Ordering};

    //   use syscalls::SyscallArgs;
    use crate::multi_receive;
    use crate::{QueueEntry, QueueError, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags};

    fn wait(x: &AtomicU64, v: u64) {
        while x.load(Ordering::SeqCst) == v {
            core::hint::spin_loop();
        }
    }

    fn wake(_x: &AtomicU64) {
        //   println!("wake");
    }

    #[test]
    fn it_transmits() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

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
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

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
        assert_eq!(res, Err(QueueError::WouldBlock));
    }

    #[test]
    fn it_nonblock_receives() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        let res = q.submit(QueueEntry::new(1, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.receive(wait, wake, ReceiveFlags::empty());
        assert!(res.is_ok());
        assert_eq!(res.unwrap().info(), 1);
        assert_eq!(res.unwrap().item(), 7);
        let res = q.receive(wait, wake, ReceiveFlags::NON_BLOCK);
        assert_eq!(res.unwrap_err(), QueueError::WouldBlock);
    }

    #[test]
    fn it_multi_receives() {
        let qh1 = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer1 = [QueueEntry::<i32>::default(); 1 << 4];
        let q1 = unsafe { RawQueue::new(&qh1, buffer1.as_mut_ptr()) };

        let qh2 = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer2 = [QueueEntry::<i32>::default(); 1 << 4];
        let q2 = unsafe { RawQueue::new(&qh2, buffer2.as_mut_ptr()) };

        let res = q1.submit(QueueEntry::new(1, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q2.submit(QueueEntry::new(2, 8), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));

        let mut output = [None, None];
        let res = multi_receive(
            &[&q1, &q2],
            &mut output,
            |_| {},
            |_| {},
            ReceiveFlags::empty(),
        );
        assert_eq!(res, Ok(2));
        assert_eq!(output[0].unwrap().info(), 1);
        assert_eq!(output[0].unwrap().item(), 7);
        assert_eq!(output[1].unwrap().info(), 2);
        assert_eq!(output[1].unwrap().item(), 8);
    }

    /*
        #[cfg(not(target_os = "twizzler"))]
        extern crate crossbeam;
        #[cfg(not(target_os = "twizzler"))]
        extern crate test;
        #[cfg(not(target_os = "twizzler"))]
        #[bench]
        fn two_threads(b: &mut test::Bencher) -> impl Termination {
            let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
            let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
            let q = unsafe {
                RawQueue::new(
                    std::mem::transmute::<&RawQueueHdr, &'static RawQueueHdr>(&qh),
                    buffer.as_mut_ptr(),
                )
            };

            //let count = AtomicU64::new(0);
            let x = crossbeam::scope(|s| {
                s.spawn(|_| loop {
                    let res = q.receive(wait, wake, ReceiveFlags::empty());
                    assert!(res.is_ok());
                    if res.unwrap().info() == 2 {
                        break;
                    }
                    //count.fetch_add(1, Ordering::SeqCst);
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
    */
}
