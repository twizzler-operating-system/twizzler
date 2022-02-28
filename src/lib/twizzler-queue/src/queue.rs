use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use twizzler::object::{CreateError, CreateSpec, Object};
use twizzler_abi::syscall::{
    sys_thread_sync, ObjectCreate, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_queue_raw::{QueueEntry, RawQueueHdr};
use twizzler_queue_raw::{QueueError, RawQueue};

pub use twizzler_queue_raw::ReceiveFlags;
pub use twizzler_queue_raw::SubmissionFlags;

pub struct Queue<S, C> {
    submission: RawQueue<S>,
    completion: RawQueue<C>,
    sub_rec_count: AtomicBool,
    com_rec_count: AtomicBool,
    object: Object<QueueBase<S, C>>,
}

#[repr(C)]
pub struct QueueBase<S, C> {
    sub_hdr: usize,
    com_hdr: usize,
    sub_buf: usize,
    com_buf: usize,
    _pd: PhantomData<(S, C)>,
}

fn get_raw_sub<S: Copy, C>(obj: &Object<QueueBase<S, C>>) -> RawQueue<S> {
    let base = obj.base_raw();
    let hdr = obj.raw_lea(base.sub_hdr);
    let buf = obj.raw_lea_mut(base.sub_buf);
    unsafe { RawQueue::new(hdr, buf) }
}

fn get_raw_com<S, C: Copy>(obj: &Object<QueueBase<S, C>>) -> RawQueue<C> {
    let base = obj.base_raw();
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
    pub fn object(&self) -> &Object<QueueBase<S, C>> {
        &self.object
    }

    pub fn create(
        create_spec: &CreateSpec,
        sub_queue_len: usize,
        com_queue_len: usize,
    ) -> Result<Self, CreateError> {
        let obj: Object<QueueBase<S, C>> = Object::create_with(create_spec, |obj| unsafe {
            // TODO: verify things
            let sub_len = (core::mem::size_of::<S>() * sub_queue_len) * 2;
            //let com_len = (core::mem::size_of::<C>() * com_queue_len) * 2;
            {
                let base: &mut QueueBase<S, C> = obj.base_raw_mut().assume_init_mut();
                base.sub_hdr = 0x1000;
                base.com_hdr = 0x2000;
                base.sub_buf = 0x3000;
                base.com_buf = 0x4000 + sub_len;
            }
            let srq: *mut RawQueueHdr = obj.raw_lea_mut(0x1000);
            let crq: *mut RawQueueHdr = obj.raw_lea_mut(0x2000);
            let l2len = sub_queue_len.next_power_of_two().log2();
            srq.write(RawQueueHdr::new(l2len as usize, core::mem::size_of::<S>()));
            let l2len = com_queue_len.next_power_of_two().log2();
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

    pub fn submit(&self, id: u32, item: S, flags: SubmissionFlags) -> Result<(), QueueError> {
        self.submission
            .submit(QueueEntry::new(id, item), wait, ring, flags)
    }

    pub fn receive(&self, flags: ReceiveFlags) -> Result<(u32, S), QueueError> {
        self.with_guard(true, || self.submission.receive(wait, ring, flags))
            .map(|qe| (qe.info(), qe.item()))
    }

    pub fn complete(&self, id: u32, item: C, flags: SubmissionFlags) -> Result<(), QueueError> {
        self.completion
            .submit(QueueEntry::new(id, item), wait, ring, flags)
    }

    pub fn get_completion(&self, flags: ReceiveFlags) -> Result<(u32, C), QueueError> {
        self.with_guard(false, || self.completion.receive(wait, ring, flags))
            .map(|qe| (qe.info(), qe.item()))
    }
}
