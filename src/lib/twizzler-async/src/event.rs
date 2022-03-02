use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};

use crate::block_on::block_on;

struct Inner<'a> {
    pt: &'a AtomicU64,
    val: u64,
}

pub(crate) struct ExtEvent<'a>(Arc<Inner<'a>>);

impl<'a> ExtEvent<'a> {
    pub fn new(pt: &'a AtomicU64, val: u64) -> Self {
        Self(Arc::new(Inner { pt, val }))
    }

    pub fn notify(&self, val: Option<u64>) {
        if let Some(val) = val {
            self.0.pt.store(val, Ordering::SeqCst);
        }

        let op = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(self.0.pt as *const AtomicU64),
            usize::MAX,
        ));
        // TODO: can we elide this?
        // TODO: check err
        let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
    }

    pub fn setup_sleep(&self, val: Option<u64>) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(self.0.pt as *const AtomicU64),
            val.unwrap_or(self.0.val),
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}

#[derive(Clone)]
pub(crate) struct FlagEvent(Arc<AtomicU64>);

impl FlagEvent {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    pub fn notify(&self) {
        self.0.store(1, Ordering::SeqCst);

        let op = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(&*self.0 as *const AtomicU64),
            usize::MAX,
        ));
        // TODO: can we elide this?
        // TODO: check err
        let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
    }

    pub fn clear(&self) -> bool {
        self.0.swap(0, Ordering::SeqCst) != 0
    }

    pub fn is_ready(&self) -> bool {
        self.0.load(Ordering::SeqCst) != 0
    }

    pub fn setup_sleep(&self) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&*self.0 as *const AtomicU64),
            0,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}
