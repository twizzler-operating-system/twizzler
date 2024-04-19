use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{
        sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
        ThreadSyncSleep, ThreadSyncWake,
    },
};
use twizzler_object::{CreateError, CreateSpec, Object};
pub use twizzler_queue_raw::{QueueBase, QueueError, ReceiveFlags, SubmissionFlags};
use twizzler_queue_raw::{QueueEntry, RawQueue, RawQueueHdr};

/// A single queue, holding two subqueues (sending and completion). Objects of type S are sent
/// across the sending queue, and completions of type C are sent back.
pub struct Queue<S, C> {
    submission: RawQueue<S>,
    completion: RawQueue<C>,
    sub_rec_count: AtomicBool,
    com_rec_count: AtomicBool,
    object: Object<QueueBase<S, C>>,
}

fn get_raw_sub<S: Copy, C>(obj: &Object<QueueBase<S, C>>) -> RawQueue<S> {
    let base = unsafe { obj.base_unchecked() };
    let hdr = obj.raw_lea(base.sub_hdr);
    let buf = obj.raw_lea_mut(base.sub_buf);
    unsafe { RawQueue::new(hdr, buf) }
}

fn get_raw_com<S, C: Copy>(obj: &Object<QueueBase<S, C>>) -> RawQueue<C> {
    let base = unsafe { obj.base_unchecked() };
    let hdr = obj.raw_lea(base.com_hdr);
    let buf = obj.raw_lea_mut(base.com_buf);
    unsafe { RawQueue::new(hdr, buf) }
}

impl<S: Copy, C: Copy> From<Object<QueueBase<S, C>>> for Queue<S, C> {
    fn from(x: Object<QueueBase<S, C>>) -> Self {
        Self {
            submission: get_raw_sub(&x),
            completion: get_raw_com(&x),
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
    pub fn object(&self) -> &Object<QueueBase<S, C>> {
        &self.object
    }

    /// Create a new Twizzler queue object.
    pub fn create(
        create_spec: &CreateSpec,
        sub_queue_len: usize,
        com_queue_len: usize,
    ) -> Result<Self, CreateError> {
        const HDR_LEN: usize = 0x1000;
        let obj: Object<QueueBase<S, C>> = Object::create_with(create_spec, |obj| unsafe {
            // TODO: verify things
            let sub_len = (core::mem::size_of::<S>() * sub_queue_len) * 2;
            //let com_len = (core::mem::size_of::<C>() * com_queue_len) * 2;
            let (sub_hdr, com_hdr) = {
                let base: &mut QueueBase<S, C> = obj.base_mut_unchecked().assume_init_mut();
                base.sub_hdr = NULLPAGE_SIZE + HDR_LEN;
                base.com_hdr = base.sub_hdr + HDR_LEN;
                base.sub_buf = base.com_hdr + HDR_LEN;
                base.com_buf = base.sub_buf + sub_len;
                (base.sub_hdr, base.com_hdr)
            };
            let srq: *mut RawQueueHdr = obj.raw_lea_mut(sub_hdr);
            let crq: *mut RawQueueHdr = obj.raw_lea_mut(com_hdr);
            let l2len = sub_queue_len.next_power_of_two().ilog2();
            srq.write(RawQueueHdr::new(l2len as usize, core::mem::size_of::<S>()));
            let l2len = com_queue_len.next_power_of_two().ilog2();
            crq.write(RawQueueHdr::new(l2len as usize, core::mem::size_of::<C>()));
        })?;
        Ok(obj.into())
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
