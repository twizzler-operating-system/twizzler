use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
        ThreadSyncSleep, ThreadSyncWake,
    },
};
pub use twizzler_queue_raw::{QueueBase, QueueError, ReceiveFlags, SubmissionFlags};
use twizzler_queue_raw::{QueueEntry, RawQueue, RawQueueHdr};
use twizzler_rt_abi::object::ObjectHandle;

/// A single queue, holding two subqueues (sending and completion). Objects of type S are sent
/// across the sending queue, and completions of type C are sent back.
pub struct Queue<S, C> {
    submission: RawQueue<S>,
    completion: RawQueue<C>,
    sub_rec_count: AtomicBool,
    com_rec_count: AtomicBool,
    object: ObjectHandle,
}

fn base<S, C>(obj: &ObjectHandle) -> &mut QueueBase<S, C> {
    unsafe {
        obj.start()
            .add(NULLPAGE_SIZE)
            .cast::<QueueBase<S, C>>()
            .as_mut()
            .unwrap()
    }
}

fn get_raw_sub<S: Copy, C>(obj: &ObjectHandle) -> RawQueue<S> {
    let base = base::<S, C>(obj);
    unsafe {
        let hdr = obj.start().add(base.sub_hdr).cast();
        let buf = obj.start().add(base.sub_buf).cast();
        RawQueue::new(hdr, buf)
    }
}

fn get_raw_com<S, C: Copy>(obj: &ObjectHandle) -> RawQueue<C> {
    let base = base::<S, C>(obj);
    unsafe {
        let hdr = obj.start().add(base.com_hdr).cast();
        let buf = obj.start().add(base.com_buf).cast();
        RawQueue::new(hdr, buf)
    }
}

impl<S: Copy, C: Copy> From<ObjectHandle> for Queue<S, C> {
    fn from(x: ObjectHandle) -> Self {
        Self {
            submission: get_raw_sub::<S, C>(&x),
            completion: get_raw_com::<S, C>(&x),
            sub_rec_count: AtomicBool::new(false),
            com_rec_count: AtomicBool::new(false),
            object: x,
        }
    }
}

fn wait(pt: &AtomicU64, val: u64) {
    let op = ThreadSync::new_sleep(ThreadSyncSleep::new(
        ThreadSyncReference::Virtual(pt as *const AtomicU64),
        val,
        ThreadSyncOp::Equal,
        ThreadSyncFlags::empty(),
    ));
    let _ = sys_thread_sync(&mut [op], None);
}

fn ring(pt: &AtomicU64) {
    let op = ThreadSync::new_wake(ThreadSyncWake::new(
        ThreadSyncReference::Virtual(pt as *const AtomicU64),
        usize::MAX,
    ));
    let _ = sys_thread_sync(&mut [op], None);
}

impl<S: Copy, C: Copy> Queue<S, C> {
    /// Get a handle to the internal object that holds the queue data.
    pub fn handle(&self) -> &ObjectHandle {
        &self.object
    }

    pub fn com_hdr(&self) -> &RawQueueHdr {
        self.completion.hdr()
    }

    pub fn sub_hdr(&self) -> &RawQueueHdr {
        self.submission.hdr()
    }

    pub fn has_pending_submission(&self) -> bool {
        self.submission.has_pending()
    }

    pub fn has_pending_completion(&self) -> bool {
        self.completion.has_pending()
    }

    pub fn has_sub_space(&self) -> bool {
        self.submission.has_space()
    }

    pub fn has_com_space(&self) -> bool {
        self.completion.has_space()
    }

    /// Create a new Twizzler queue object.
    pub fn init(obj: &ObjectHandle, sub_queue_len: usize, com_queue_len: usize) {
        /// Cache line. Every component starts on one so that no two things written by different
        /// threads can land in the same line — the header's internal padding would be pointless if
        /// a buffer's first entry shared a line with the previous header's `tail`.
        const LINE: usize = 64;
        const fn align_up(x: usize, a: usize) -> usize {
            (x + a - 1) & !(a - 1)
        }
        // Each of the base struct and the two headers used to get a 4 KiB region of its own, so a
        // duplex queue spanned four pages, three of them all but empty. That is invisible with one
        // hot queue and expensive with many: a thread servicing 128 of them ran ~40% slower purely
        // on address translation (`page_layout_report` in twizzler-queue-raw). Packed, a small
        // queue's headers and both buffers fit in a single page.
        const _: () = assert!(LINE % core::mem::align_of::<RawQueueHdr>() == 0);
        assert_eq!(NULLPAGE_SIZE % LINE, 0);

        // The algorithm indexes QueueEntry<S>, not S, and rounds the slot count up to a power of
        // two. Budgeting `size_of::<S>() * sub_queue_len` instead silently overlaps the completion
        // buffer with the tail of the submission buffer for small payloads or non-power-of-two
        // lengths.
        let sub_slots = sub_queue_len.next_power_of_two();
        let com_slots = com_queue_len.next_power_of_two();
        let sub_len = core::mem::size_of::<QueueEntry<S>>() * sub_slots;
        let com_len = core::mem::size_of::<QueueEntry<C>>() * com_slots;
        let (sub_hdr, com_hdr) = {
            let base: &mut QueueBase<S, C> = unsafe {
                obj.start()
                    .add(NULLPAGE_SIZE)
                    .cast::<QueueBase<S, C>>()
                    .as_mut()
                    .unwrap()
            };
            base.sub_hdr = align_up(
                NULLPAGE_SIZE + core::mem::size_of::<QueueBase<S, C>>(),
                LINE,
            );
            base.com_hdr = base.sub_hdr + core::mem::size_of::<RawQueueHdr>();
            base.sub_buf = align_up(base.com_hdr + core::mem::size_of::<RawQueueHdr>(), LINE);
            base.com_buf = align_up(base.sub_buf + sub_len, LINE);
            // True by construction, and asserted anyway: item 6 in the audit log was precisely a
            // pair of regions that overlapped because the arithmetic was subtly wrong, and it
            // corrupted silently rather than failing.
            let hdr_size = core::mem::size_of::<RawQueueHdr>();
            assert!(base.sub_hdr + hdr_size <= base.com_hdr);
            assert!(base.com_hdr + hdr_size <= base.sub_buf);
            assert!(base.sub_buf + sub_len <= base.com_buf);
            assert!(base.com_buf + com_len <= MAX_SIZE - NULLPAGE_SIZE);
            (base.sub_hdr, base.com_hdr)
        };
        unsafe {
            let srq: *mut RawQueueHdr = obj.start().add(sub_hdr).cast();
            let crq: *mut RawQueueHdr = obj.start().add(com_hdr).cast();
            srq.write(RawQueueHdr::new(
                sub_slots.ilog2() as usize,
                core::mem::size_of::<QueueEntry<S>>(),
            ));
            crq.write(RawQueueHdr::new(
                com_slots.ilog2() as usize,
                core::mem::size_of::<QueueEntry<C>>(),
            ));
        }
    }

    fn with_guard<R>(&self, sub: bool, f: impl FnOnce() -> R) -> R {
        let guard = if sub {
            &self.sub_rec_count
        } else {
            &self.com_rec_count
        };
        if guard.swap(true, Ordering::SeqCst) {
            panic!("cannot call queue receive operations from multiple concurrent threads");
        }
        let res = f();
        guard.store(false, Ordering::SeqCst);
        res
    }

    /// Submit an item of type S across the sending subqueue, with a given id.
    pub fn submit(&self, id: u32, item: S, flags: SubmissionFlags) -> Result<(), QueueError> {
        self.submission
            .submit(QueueEntry::new(id, item), wait, ring, flags)
    }

    /// Receive an item and request id from the sending subqueue.
    pub fn receive(&self, flags: ReceiveFlags) -> Result<(u32, S), QueueError> {
        self.with_guard(true, || self.submission.receive(wait, ring, flags))
            .map(|qe| (qe.info(), qe.item()))
    }

    /// Submit a completion item of type C across the completion subqueue.
    pub fn complete(&self, id: u32, item: C, flags: SubmissionFlags) -> Result<(), QueueError> {
        self.completion
            .submit(QueueEntry::new(id, item), wait, ring, flags)
    }

    /// Receive a completion item and id from the completion subqueue.
    pub fn get_completion(&self, flags: ReceiveFlags) -> Result<(u32, C), QueueError> {
        self.with_guard(false, || self.completion.receive(wait, ring, flags))
            .map(|qe| (qe.info(), qe.item()))
    }

    #[inline]
    fn build_thread_sync(ptr: &AtomicU64, val: u64) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(ptr as *const AtomicU64),
            val,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    /// Setup a sleep operation for reading the completion subqueue.
    #[inline]
    pub fn setup_read_com_sleep(&self) -> ThreadSyncSleep {
        let (ptr, val) = self.completion.setup_sleep_simple();
        Self::build_thread_sync(ptr, val)
    }

    /// Setup a sleep operation for reading the sending subqueue.
    #[inline]
    pub fn setup_read_sub_sleep(&self) -> ThreadSyncSleep {
        let (ptr, val) = self.submission.setup_sleep_simple();
        Self::build_thread_sync(ptr, val)
    }

    /// Setup a sleep operation for writing the sending subqueue.
    #[inline]
    pub fn setup_write_sub_sleep(&self) -> ThreadSyncSleep {
        let (ptr, val) = self.submission.setup_send_sleep_simple();
        Self::build_thread_sync(ptr, val)
    }

    /// Setup a sleep operation for writing the completion subqueue.
    #[inline]
    pub fn setup_write_com_sleep(&self) -> ThreadSyncSleep {
        let (ptr, val) = self.completion.setup_send_sleep_simple();
        Self::build_thread_sync(ptr, val)
    }
}
