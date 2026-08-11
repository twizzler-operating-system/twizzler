use alloc::{
    collections::{btree_map::BTreeMap, btree_set::BTreeSet},
    vec::Vec,
};
use core::fmt::Debug;

use twizzler_abi::{object::ObjID, syscall::ObjectStats};

use super::ObjectRef;
use crate::mutex::Mutex;

pub struct TiesStatic {
    inner: Mutex<Ties<ObjID, ObjectRef>>,
}

impl TiesStatic {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(Ties::new()),
        }
    }

    pub fn delete_object(&self, obj: ObjectRef) {
        // Dropping the last `ObjectRef` runs `Object::drop`, which calls the *blocking*
        // `pager::del_object`. Doing that under the ties mutex sleeps a thread on a pager round
        // trip with `Ties` held, and `Ties` is taken from the object-teardown path the idle
        // threads run while reaping -- so every reaper queues behind one pager request. Hand the
        // released refs back and drop them after the guard is gone.
        let released = self.inner.lock().delete_value(obj.id(), obj);
        drop(released);
    }

    pub fn create_object_ties(&self, created_id: ObjID, ties: impl IntoIterator<Item = ObjID>) {
        let ties = ties.into_iter().collect::<Vec<_>>();
        if ties.is_empty() {
            return;
        }
        self.inner.lock().insert_ties(created_id, ties);
    }

    pub fn lookup_object(&self, id: ObjID) -> Option<ObjectRef> {
        self.inner.lock().lookup_deleted(id)
    }

    pub fn with_deleted_map<R>(&self, f: impl FnOnce(&BTreeMap<ObjID, ObjectRef>) -> R) -> R {
        f(&self.inner.lock().pending_delete)
    }
}

pub(super) static TIE_MGR: TiesStatic = TiesStatic::new();

#[derive(Default)]
struct Ties<Key, Value> {
    ties: BTreeMap<Key, BTreeSet<Key>>,
    pending_delete: BTreeMap<Key, Value>,
}

impl<K: Ord + PartialOrd + PartialEq + Debug + Copy + Clone, V: Debug> Ties<K, V> {
    const fn new() -> Self {
        Self {
            ties: BTreeMap::new(),
            pending_delete: BTreeMap::new(),
        }
    }

    pub fn insert_ties(&mut self, obj: K, deps: impl IntoIterator<Item = K>) {
        for val in deps.into_iter() {
            self.ties.entry(obj).or_default().insert(val);
        }
    }

    fn remove_tie(&mut self, obj: K, tie: K) {
        self.ties.entry(obj).or_default().remove(&tie);
    }

    fn remove_all_ties(&mut self, obj: K) {
        self.ties.entry(obj).or_default().clear();
    }

    /// Returns the values whose last tie just went away, for the caller to drop. See
    /// `TiesStatic::delete_object` for why they are not dropped here.
    #[must_use = "released values must be dropped outside the ties lock"]
    fn delete_ties(&mut self, target: K) -> Vec<V> {
        let mut released = Vec::new();
        for (objid, set) in self.ties.iter_mut() {
            set.remove(&target);
            if set.is_empty() {
                released.extend(self.pending_delete.remove(&objid));
            }
        }
        released
    }

    /// Returns the values that are now unreferenced, for the caller to drop outside the lock.
    #[must_use = "released values must be dropped outside the ties lock"]
    pub fn delete_value(&mut self, id: K, val: V) -> Vec<V> {
        let mut released = self.delete_ties(id);
        let _ = self
            .ties
            .extract_if(.., |_, val| val.is_empty())
            .collect::<Vec<_>>();
        if self.ties.get(&id).map_or(0, |set| set.len()) > 0 {
            self.pending_delete.insert(id, val);
        } else {
            released.push(val);
        }
        released
    }
}

impl<K: Ord + PartialOrd + PartialEq + Debug + Copy + Clone, V: Clone> Ties<K, V> {
    pub fn lookup_deleted(&self, id: K) -> Option<V> {
        self.pending_delete.get(&id).cloned()
    }
}

pub fn fill_stats(stats: &mut ObjectStats) {
    let ties = TIE_MGR.inner.lock();
    stats.nr_ties = ties.ties.len();
    stats.nr_pending_delete = ties.pending_delete.len();
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use twizzler_kernel_macros::kernel_test;

    use super::*;

    struct Bar {
        id: u32,
        dest: Arc<AtomicBool>,
    }

    impl Debug for Bar {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("Bar")
                .field("id", &self.id)
                .finish_non_exhaustive()
        }
    }

    impl Drop for Bar {
        fn drop(&mut self) {
            self.dest.store(true, Ordering::SeqCst);
        }
    }

    static BAR_ID: AtomicU32 = AtomicU32::new(1);
    impl Default for Bar {
        fn default() -> Self {
            Self::new(
                Arc::new(AtomicBool::default()),
                BAR_ID.fetch_add(1, core::sync::atomic::Ordering::SeqCst),
            )
        }
    }

    impl Bar {
        fn new(dest: Arc<AtomicBool>, id: u32) -> Self {
            Self { dest, id }
        }

        fn tracker(&self) -> BarTracker {
            BarTracker {
                id: self.id,
                tracker: self.dest.clone(),
            }
        }
    }

    struct BarTracker {
        id: u32,
        tracker: Arc<AtomicBool>,
    }

    impl BarTracker {
        fn is_destroyed(&self) -> bool {
            self.tracker.load(Ordering::SeqCst)
        }
    }

    fn test_ties(ties: &mut Ties<u32, Bar>) {
        let x = Bar::default();
        let x_tracker = x.tracker();
        let y = Bar::default();
        let y_tracker = y.tracker();
        let z = Bar::default();
        let z_tracker = z.tracker();
        let zz = Bar::default();
        let zz_tracker = zz.tracker();
        ties.insert_ties(y.id, [x.id]);
        ties.insert_ties(z.id, [y.id]);
        ties.insert_ties(zz.id, [y.id]);

        // Dropping the returned values is what destroys them now, so these mirror what
        // `TiesStatic::delete_object` does once the lock is released.
        drop(ties.delete_value(z.id, z));
        drop(ties.delete_value(y.id, y));
        drop(ties.delete_value(zz.id, zz));

        assert!(!x_tracker.is_destroyed());
        assert!(!y_tracker.is_destroyed());
        assert!(z_tracker.is_destroyed());
        assert!(zz_tracker.is_destroyed());

        drop(ties.delete_value(x.id, x));

        assert!(x_tracker.is_destroyed());
        assert!(y_tracker.is_destroyed());
    }

    #[kernel_test]
    fn test_ties_kt() {
        let mut ties = Ties::default();
        test_ties(&mut ties);
    }
}
