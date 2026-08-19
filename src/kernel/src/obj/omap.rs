//! Sharded object map: a per-shard spinlock over an intrusive RBTree keyed by object id.
//!
//! Replaces the single kernel-wide `Mutex<BTreeMap>` whose insert convoyed contended create
//! (`register` measured at 14.8us/create at smp4, tag `createprof2`) and whose full-map walks
//! stalled every `lookup_object` in the kernel. Ids are content-derived hashes, so their low
//! bits index shards uniformly with no hash step.
//!
//! Shard locks are leaf locks. Inside a hold: tree operations and `Arc` *clones* only — no
//! allocation (the link is intrusive, so insert allocates nothing), no other locks, and no
//! `Arc` *drops* (a drop can run the object destructor). Every method that evicts an entry
//! returns the reference for the caller to drop outside the lock, and the scan paths claim
//! bounded chunks per hold so no walk pins a shard.

use alloc::{sync::Arc, vec::Vec};

use intrusive_collections::{Bound, KeyAdapter, RBTree, RBTreeAtomicLink, intrusive_adapter};
use twizzler_abi::object::ObjID;

use super::{Object, ObjectRef};
use crate::spinlock::Spinlock;

pub const SHARDS: usize = 64;

/// Entries claimed per lock hold on scan paths, bounding hold time without allocating under
/// the lock.
const SCAN_CHUNK: usize = 64;

intrusive_adapter!(pub OmapAdapter = ObjectRef: Object { omap_link: RBTreeAtomicLink });

impl<'a> KeyAdapter<'a> for OmapAdapter {
    type Key = ObjID;

    fn get_key(&self, o: &'a Object) -> ObjID {
        o.id
    }
}

pub struct ShardedOmap {
    shards: [Spinlock<RBTree<OmapAdapter>>; SHARDS],
}

impl ShardedOmap {
    pub fn new() -> Self {
        Self {
            shards: core::array::from_fn(|_| Spinlock::new(RBTree::new(OmapAdapter::new()))),
        }
    }

    fn shard(&self, id: ObjID) -> &Spinlock<RBTree<OmapAdapter>> {
        &self.shards[id.raw() as usize % SHARDS]
    }

    /// Insert, replacing any existing entry for this id (the `BTreeMap::insert` semantics the
    /// old map had). The evicted reference, if any, is returned to drop outside the lock.
    pub fn insert(&self, obj: ObjectRef) -> Option<ObjectRef> {
        let id = obj.id;
        let mut tree = self.shard(id).lock();
        let mut cursor = tree.find_mut(&id);
        if cursor.is_null() {
            tree.insert(obj);
            None
        } else {
            cursor.replace_with(obj).ok()
        }
    }

    pub fn lookup(&self, id: ObjID) -> Option<ObjectRef> {
        self.shard(id).lock().find(&id).clone_pointer()
    }

    /// Remove the entry for `id` iff it is still this exact object and still marked for
    /// delete — the caller's reap predicate ran unlocked and may have raced a replacement or a
    /// resurrection. Returns the removed reference to drop outside the lock.
    pub fn remove_if_pending(&self, id: ObjID, obj: &ObjectRef) -> Option<ObjectRef> {
        let mut tree = self.shard(id).lock();
        let mut cursor = tree.find_mut(&id);
        let unchanged = cursor
            .get()
            .is_some_and(|cur| core::ptr::eq(cur, Arc::as_ptr(obj)) && cur.is_pending_delete());
        if unchanged { cursor.remove() } else { None }
    }

    /// All pending-delete objects, for the reaper's unlocked-predicate phase. Claims
    /// [SCAN_CHUNK] per hold and appends outside the lock.
    pub fn collect_pending(&self, out: &mut Vec<(ObjID, ObjectRef)>) {
        let mut chunk = heapless::Vec::<ObjectRef, SCAN_CHUNK>::new();
        self.for_chunks(|o| o.is_pending_delete(), &mut chunk, |chunk, out| {
            out.extend(chunk.drain(..).map(|o| (o.id, o)));
        }, out);
    }

    /// Every id, shard-major ascending. Same claim/append shape as [Self::collect_pending];
    /// the enumerate caller applies its own offset/limit after the ties manager's deleted map
    /// is chained on.
    pub fn collect_ids(&self, out: &mut Vec<ObjID>) {
        let mut chunk = heapless::Vec::<ObjectRef, SCAN_CHUNK>::new();
        self.for_chunks(|_| true, &mut chunk, |chunk, out| {
            out.extend(chunk.drain(..).map(|o| o.id));
        }, out);
    }

    /// Every object, for diagnostics ([super::print_all_objects]).
    pub fn collect_all(&self, out: &mut Vec<ObjectRef>) {
        let mut chunk = heapless::Vec::<ObjectRef, SCAN_CHUNK>::new();
        self.for_chunks(|_| true, &mut chunk, |chunk, out| {
            out.extend(chunk.drain(..));
        }, out);
    }

    /// Walk every shard claiming matching entries one bounded chunk per hold, flushing each
    /// chunk through `flush` after the hold is released. The resume key is inclusive: the
    /// entry that overflowed a chunk is re-found by the next hold (or skipped past if it was
    /// removed in between — either outcome is sound for these callers).
    fn for_chunks<T>(
        &self,
        pred: impl Fn(&Object) -> bool,
        chunk: &mut heapless::Vec<ObjectRef, SCAN_CHUNK>,
        flush: impl Fn(&mut heapless::Vec<ObjectRef, SCAN_CHUNK>, &mut Vec<T>),
        out: &mut Vec<T>,
    ) {
        for shard in &self.shards {
            let mut resume: Option<ObjID> = None;
            loop {
                {
                    let tree = shard.lock();
                    let mut cursor = match resume.take() {
                        Some(id) => tree.lower_bound(Bound::Included(&id)),
                        None => tree.front(),
                    };
                    while let Some(o) = cursor.get() {
                        if pred(o) {
                            if chunk.is_full() {
                                resume = Some(o.id);
                                break;
                            }
                            let _ = chunk.push(cursor.clone_pointer().unwrap());
                        }
                        cursor.move_next();
                    }
                }
                flush(chunk, out);
                if resume.is_none() {
                    break;
                }
            }
        }
    }
}
