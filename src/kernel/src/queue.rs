use core::{fmt::Debug, sync::atomic::AtomicU64};

use alloc::collections::BTreeMap;
use twizzler_abi::{
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
        ThreadSyncWake,
    },
};
use twizzler_queue_raw::{
    QueueEntry, QueueError, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags,
};

use crate::{
    idcounter::{Id, IdCounter},
    mutex::Mutex,
    syscall::sync::sys_thread_sync,
};

struct Outstanding<D, C> {
    id: Id<'static>,
    data: D,
    callback: fn(D, C),
}

pub struct Queue<S, C, D> {
    id: ObjID,
    slot: usize,
    raw_sub: RawQueue<S>,
    raw_cmp: RawQueue<C>,
    infos: IdCounter,
    outstanding: Mutex<BTreeMap<u32, Outstanding<D, C>>>,
}

#[derive(Debug)]
// TODO: Get this from twizzler-abi.
#[repr(C)]
pub struct QueueBase {
    sub_hdr: usize,
    com_hdr: usize,
    sub_buf: usize,
    com_buf: usize,
}

impl<S: Copy, C: Copy + Debug, D> Queue<S, C, D> {
    fn wait(word: &AtomicU64, val: u64) {
        let op = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(word),
            val,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        sys_thread_sync(&mut [op], None).unwrap();
    }
    fn ring(word: &AtomicU64) {
        let op = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(word),
            usize::MAX,
        ));
        sys_thread_sync(&mut [op], None).unwrap();
    }

    pub unsafe fn init_from_slots(id: ObjID, slot: usize) -> Self {
        let vaddr = slot * MAX_SIZE;
        let hdr = ((vaddr + NULLPAGE_SIZE) as *const QueueBase)
            .as_ref()
            .unwrap();
        Self {
            id,
            slot,
            raw_sub: RawQueue::new(
                (vaddr + hdr.sub_hdr) as *const RawQueueHdr,
                (vaddr + hdr.sub_buf) as *mut QueueEntry<S>,
            ),
            raw_cmp: RawQueue::new(
                (vaddr + hdr.com_hdr) as *const RawQueueHdr,
                (vaddr + hdr.com_buf) as *mut QueueEntry<C>,
            ),
            infos: IdCounter::new(),
            outstanding: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn receive(&self, flags: ReceiveFlags) -> Result<QueueEntry<S>, QueueError> {
        self.raw_sub.receive(Self::wait, Self::ring, flags)
    }

    pub fn complete(&self, info: u32, cmp: C, flags: SubmissionFlags) -> Result<(), QueueError> {
        self.raw_cmp
            .submit(QueueEntry::new(info, cmp), Self::wait, Self::ring, flags)
    }

    pub fn handle_reqs(&self, handler: fn(item: S) -> C) {
        while let Ok(item) = self.receive(ReceiveFlags::empty()) {
            let info = item.info();
            let resp = handler(item.item());
            self.complete(info, resp, SubmissionFlags::empty()).unwrap();
        }
    }

    pub fn process_completions(&self, justone: bool, flags: ReceiveFlags) {
        while let Ok(entry) = self.raw_cmp.receive(Self::wait, Self::ring, flags) {
            let mut outstanding = self.outstanding.lock();
            if let Some(out) = outstanding.remove(&entry.info()) {
                (out.callback)(out.data, entry.item());
            } else {
                logln!("failed to process completion on queue: {:?}", entry);
            }
            if justone {
                break;
            }
        }
    }

    pub fn submit(
        &'static self,
        item: S,
        data: D,
        on_complete: fn(D, C),
        flags: SubmissionFlags,
    ) -> Result<(), QueueError> {
        self.process_completions(true, ReceiveFlags::NON_BLOCK);
        let id = self.infos.next();
        let n = id.value() as u32;
        self.outstanding.lock().insert(
            n,
            Outstanding {
                id,
                data,
                callback: on_complete,
            },
        );
        let entry = QueueEntry::new(n, item);
        self.raw_sub.submit(entry, Self::wait, Self::ring, flags)
    }
}
