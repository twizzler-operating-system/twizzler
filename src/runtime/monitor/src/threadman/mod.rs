use std::{
    collections::HashMap,
    mem::MaybeUninit,
    sync::{Arc, Mutex},
};

use twizzler_abi::syscall::ThreadSyncSleep;
use twizzler_runtime_api::ObjID;

use self::thread_cleaner::ThreadCleaner;

mod thread_cleaner;

#[allow(dead_code)]
struct ManagedThread {
    id: ObjID,
    super_stack: Box<[MaybeUninit<u8>]>,
}

impl Drop for ManagedThread {
    fn drop(&mut self) {
        tracing::trace!("dropping ManagedThread {}", self.id);
    }
}

pub type ManagedThreadRef = Arc<ManagedThread>;

impl ManagedThread {
    fn new(id: ObjID, super_stack: Box<[MaybeUninit<u8>]>) -> ManagedThreadRef {
        Arc::new(Self { id, super_stack })
    }

    fn waitable_until_exit(&self) -> ThreadSyncSleep {
        todo!()
    }

    fn has_exited(&self) -> bool {
        todo!()
    }
}

#[derive(Default)]
struct ThreadManagerInner {
    all: HashMap<ObjID, ManagedThreadRef>,
    cleaner: Option<ThreadCleaner>,
}

impl ThreadManagerInner {
    fn get_cleaner_thread(&mut self) -> &ThreadCleaner {
        self.cleaner.get_or_insert(ThreadCleaner::new())
    }
}

pub struct ThreadManager {
    inner: Mutex<ThreadManagerInner>,
}

lazy_static::lazy_static! {
pub static ref THREAD_MGR: ThreadManager = ThreadManager { inner: Mutex::new(ThreadManagerInner::default())};
}

impl ThreadManager {
    pub fn insert(&self, th: ManagedThreadRef) {
        let mut inner = self.inner.lock().unwrap();
        inner.all.insert(th.id, th.clone());
        inner.get_cleaner_thread().track(th);
    }

    fn do_remove(&self, th: &ManagedThreadRef) {
        let mut inner = self.inner.lock().unwrap();
        inner.all.remove(&th.id);
    }

    pub fn remove(&self, th: &ManagedThreadRef) {
        let mut inner = self.inner.lock().unwrap();
        inner.get_cleaner_thread().untrack(th.id);
        inner.all.remove(&th.id);
    }

    pub fn get(&self, id: ObjID) -> Option<ManagedThreadRef> {
        self.inner.lock().unwrap().all.get(&id).cloned()
    }
}
