use core::{
    fmt::Display,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::once::Once;
use alloc::vec::Vec;

use crate::mutex::Mutex;

pub struct IdCounter {
    counter: AtomicU64,
    reuse: Once<Mutex<Vec<u64>>>,
}

pub struct Id<'a> {
    id: u64,
    counter: &'a IdCounter,
}

impl IdCounter {
    pub const fn new() -> Self {
        Self {
            counter: AtomicU64::new(1),
            reuse: Once::new(),
        }
    }

    pub fn next(&self) -> Id<'_> {
        /* TODO: use try lock */
        let reuser = self.reuse.poll();
        if let Some(reuser) = reuser {
            let mut reuser = reuser.lock();
            if let Some(id) = reuser.pop() {
                return Id { id, counter: self };
            }
        }
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        Id { id, counter: self }
    }

    fn release(&self, id: u64) {
        assert!(id > 0);
        self.reuse.call_once(|| Mutex::new(Vec::new()));
        //TODO: we could optimize here by trying to subtract from ID_COUNTER using CAS if the thread ID
        //is the current top value of the counter
        let mut reuser = self.reuse.wait().lock();
        reuser.push(id);
    }
}

impl<'a> Drop for Id<'a> {
    fn drop(&mut self) {
        self.counter.release(self.id);
    }
}

impl Display for Id<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.id)
    }
}

impl core::fmt::Debug for Id<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Id({})", self.id)
    }
}

impl PartialEq for Id<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Id<'_> {}

impl PartialOrd for Id<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for Id<'_> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

impl Id<'_> {
    pub fn value(&self) -> u64 {
        self.id
    }
}

pub trait StableId {
    fn id(&self) -> &Id<'_>;
}
