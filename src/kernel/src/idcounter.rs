use core::{
    fmt::Display,
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

/// Hands out unique u64 ids. **Ids are never reused.**
///
/// The reuse pool this used to keep was a `Once<Mutex<Vec<u64>>>`, so returning an id allocated and
/// took a *sleeping* mutex. That forced every path that might drop the last reference to an
/// id-holder to prove it was neither in a critical section nor holding a spinlock -- see the
/// deferred-drop machinery in `thread::reaper`, `syscall::sync`, `obj::thread_sync` and
/// `processor::sched`. Recycling also had to be defended against by consumers: `VirtContext` keeps
/// a separate never-reusing `memo_tag`, and `Mutex`'s owner check compares objids rather than ids,
/// both because a dead holder's id could be handed to a live one.
///
/// A u64 that only ascends costs nothing to give away, so nobody has to defend against any of it.
pub struct IdCounter {
    counter: AtomicU64,
}

impl Default for IdCounter {
    fn default() -> Self {
        // Not `AtomicU64::default()`: ids must be non-zero. `mutex.rs` uses 0 as its "no current
        // thread" sentinel precisely because an id can never be 0.
        Self::new()
    }
}

pub struct Id<'a> {
    id: u64,
    // Borrows the counter purely so an id cannot outlive it. Nothing is returned on drop.
    _counter: PhantomData<&'a IdCounter>,
}

pub struct SimpleId {
    id: u64,
}

impl SimpleId {
    pub fn value(&self) -> u64 {
        self.id
    }
}

impl From<u32> for SimpleId {
    fn from(value: u32) -> Self {
        Self { id: value as u64 }
    }
}
impl From<u64> for SimpleId {
    fn from(value: u64) -> Self {
        Self { id: value }
    }
}

impl IdCounter {
    pub const fn new() -> Self {
        Self {
            counter: AtomicU64::new(1),
        }
    }

    pub fn next(&self) -> Id<'_> {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        Id {
            id,
            _counter: PhantomData,
        }
    }

    pub fn next_simple(&self) -> SimpleId {
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        SimpleId { id }
    }

    /// No-op: ids are not reused. Kept so callers can keep expressing that they are done with one.
    pub fn release_simple(&self, _id: SimpleId) {}
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
