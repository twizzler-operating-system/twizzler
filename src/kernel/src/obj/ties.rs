use alloc::{
    collections::{btree_map::BTreeMap, btree_set::BTreeSet},
    vec::Vec,
};
use core::fmt::Debug;

use twizzler_abi::object::ObjID;

use super::ObjectRef;
use crate::{mutex::Mutex, thread::current_memory_context};

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
        let (c, p) = {
            (
                obj.contexts.lock().contexts.len(),
                obj.pin_info.lock().pins.len(),
            )
        };
        //logln!("ties: tracking object: {} ({} {})", obj.id(), c, p);
        self.inner.lock().delete_value(obj.id(), obj);
    }

    pub fn create_object_ties(&self, created_id: ObjID, ties: impl IntoIterator<Item = ObjID>) {
        let ties = ties.into_iter().collect::<Vec<_>>();
        if ties.is_empty() {
            return;
        }
        //logln!("ties: setting: {} => {:?}", created_id, ties);
        self.inner.lock().insert_ties(created_id, ties);
    }

    pub fn lookup_object(&self, id: ObjID) -> Option<ObjectRef> {
        self.inner.lock().lookup_deleted(id)
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

    fn delete_ties(&mut self, target: K) {
        for (objid, set) in self.ties.iter_mut() {
            set.remove(&target);
            if set.is_empty() {
                self.pending_delete.remove(&objid);
            }
        }
    }

    pub fn delete_value(&mut self, id: K, val: V) {
        self.delete_ties(id);
        let _ = self
            .ties
            .extract_if(|_, val| val.is_empty())
            .collect::<Vec<_>>();
        if self.ties.get(&id).map_or(0, |set| set.len()) > 0 {
            self.pending_delete.insert(id, val);
        }
    }
}

impl<K: Ord + PartialOrd + PartialEq + Debug + Copy + Clone, V: Clone> Ties<K, V> {
    pub fn lookup_deleted(&self, id: K) -> Option<V> {
        self.pending_delete.get(&id).cloned()
    }
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

        ties.delete_value(z.id, z);
        ties.delete_value(y.id, y);
        ties.delete_value(zz.id, zz);

        assert!(!x_tracker.is_destroyed());
        assert!(!y_tracker.is_destroyed());
        assert!(z_tracker.is_destroyed());
        assert!(zz_tracker.is_destroyed());

        ties.delete_value(x.id, x);

        assert!(x_tracker.is_destroyed());
        assert!(y_tracker.is_destroyed());
    }

    #[kernel_test]
    fn test_ties_kt() {
        let mut ties = Ties::default();
        test_ties(&mut ties);
    }
}
