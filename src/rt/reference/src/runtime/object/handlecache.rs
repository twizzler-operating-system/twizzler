use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use tracing::trace;
use twizzler_abi::{
    object::MAX_SIZE,
    syscall::{sys_map_ctrl, MapControlCmd},
};
use twizzler_rt_abi::{
    bindings::{object_handle, release_flags, RELEASE_NO_CACHE},
    object::MapFlags,
};

use super::free_runtime_info;
use crate::runtime::object::{ObjectMapKey, RuntimeHandleInfo};

type Mapping = super::ObjectMapKey;

const QUEUE_LEN: usize = 96;

/// How long a released handle stays cached before it is unmapped anyway.
///
/// This is a bound on staleness, not a performance knob. A cached handle is a mapping the kernel
/// cannot tell from a live one, and `obj::scan_deleted` reaps an object only once its map count
/// reaches zero -- so for as long as an entry sits here, an object deleted by anyone (this
/// compartment or, more usually, naming-srv) keeps its pages and, if persistent, its backing
/// store. Before this existed the only bound was the queue length: an idle entry was held until
/// `QUEUE_LEN` other handles happened to be released, which for a compartment that goes quiet is
/// no bound at all.
///
/// Two seconds is long enough to cover close-then-reopen and repeated lookups of the same path
/// (the namer's own memo expires at one), and short enough that a deleted object's storage comes
/// back on a human timescale.
const IDLE_TTL: Duration = Duration::from_secs(2);

#[derive(Default, Debug, Clone)]
pub struct HandleCache {
    active: BTreeMap<Mapping, object_handle>,
    /// Released handles keyed by release sequence, each stamped when it was released.
    ///
    /// Keyed rather than a `VecDeque` because `activate` has to find an entry *by mapping*, and
    /// scanning for it ran the length of the queue while holding the manager mutex -- which is the
    /// lock every `map_object` takes, cache hits included. Raising `QUEUE_LEN` to 96 made that
    /// scan three times longer, so the two changes pull against each other unless this is a
    /// lookup.
    ///
    /// The sequence is monotonic, so iteration order is still release order: the first entry is
    /// the least recently released, which is what expiry and eviction both need.
    queued: BTreeMap<u64, (Mapping, object_handle, Instant)>,
    /// Where each queued mapping sits, so `activate` is two lookups rather than a scan.
    queued_at: BTreeMap<Mapping, u64>,
    next_seq: u64,
    slotmap: BTreeMap<usize, Mapping>,
    /// Mappings whose monitor unmap gate call still has to be made. The cache never calls the
    /// monitor itself: the gate has to happen with the manager mutex dropped, and only the manager
    /// can mark the key in-flight first so a concurrent map of it waits rather than racing.
    pending_unmaps: Vec<Mapping>,
}

// Safety: this is needed because of the raw pointers in object_handle, but that's okay here
// because those pointers are not used within the handle cache.
unsafe impl Send for HandleCache {}

impl HandleCache {
    pub const fn new() -> Self {
        Self {
            active: BTreeMap::new(),
            queued: BTreeMap::new(),
            queued_at: BTreeMap::new(),
            next_seq: 0,
            slotmap: BTreeMap::new(),
            pending_unmaps: Vec::new(),
        }
    }

    /// If map is present in either the active or the inactive lists, return a mutable reference to
    /// it. If the handle was inactive, move it to the active list.
    pub fn activate(&mut self, map: Mapping) -> Option<object_handle> {
        if let Some(seq) = self.queued_at.remove(&map) {
            trace!("activate {:?} from queue seq {}", map, seq);
            // Unwrap-Ok: `queued_at` and `queued` gain and lose an entry together.
            let (_, handle, _) = self.queued.remove(&seq).unwrap();
            if MapFlags::from_bits_truncate(handle.map_flags).contains(MapFlags::INDIRECT)
                && sys_map_ctrl(handle.start.cast(), MAX_SIZE, MapControlCmd::Update, 0).is_err()
            {
                // Failed to reactivate -- don't leak the handle or leave slotmap pointing at
                // a mapping that's in neither queued nor active.
                self.do_remove(&handle);
                return None;
            }
            (unsafe { &*handle.runtime_info.cast::<RuntimeHandleInfo>() })
                .fot_cache
                .clear();
            // Not `insert`: that also writes the slot index, and `release` leaves a queued entry's
            // row in place -- only `do_remove` clears it -- so the row is already correct and
            // re-inserting it is a descent that finds the key it is looking for. And the handle is
            // in hand, so returning it here saves reading back what was just inserted:
            // `BTreeMap::insert` yields the *old* value, which is what made that necessary.
            let _r = self.active.insert(map, handle);
            debug_assert!(_r.is_none());
            return Some(handle);
        }
        trace!("activate {:?}", map);
        self.active.get_mut(&map).map(|item| *item)
    }

    /// Activate, using a slot as key.
    pub fn activate_from_ptr(&mut self, ptr: *const u8) -> Option<object_handle> {
        let slot = (ptr as usize) / MAX_SIZE;
        trace!("activate-from-ptr: {:p} (slot = {})", ptr, slot);
        let map = self.slotmap.get(&slot)?;
        self.activate(*map)
    }

    /// Insert a handle into the active list. Item must not be already mapped.
    pub fn insert(&mut self, handle: object_handle) {
        let slot = (handle.start as usize) / MAX_SIZE;
        let map = ObjectMapKey::from_raw_handle(&handle);
        trace!("insert {:?}", map);
        let _r = self.active.insert(map, handle);
        debug_assert!(_r.is_none());
        self.slotmap.insert(slot, map);
    }

    /// Drop the cache's ownership of `item` and queue its unmap for the manager to issue.
    ///
    /// No one else has a reference outside of the runtime, and we are called only after the handle
    /// has been bumped from the queue, so the internal refs had to be zero and never incremented.
    fn do_remove(&mut self, item: &object_handle) {
        let slot = (item.start as usize) / MAX_SIZE;
        let map = Mapping::from_raw_handle(item);
        free_runtime_info(item.runtime_info.cast());
        self.slotmap.remove(&slot);
        self.pending_unmaps.push(map);
    }

    /// Take the unmaps queued by any operation since the last drain.
    pub fn take_pending_unmaps(&mut self) -> Vec<Mapping> {
        core::mem::take(&mut self.pending_unmaps)
    }

    /// Put back unmaps the manager could not claim, for a later drain.
    pub fn requeue_unmaps(&mut self, keys: Vec<Mapping>) {
        self.pending_unmaps.extend(keys);
    }

    /// Void a queued unmap for `map`, because the mapping is live again.
    ///
    /// A queued unmap says "the runtime no longer holds this key". A map that completes afterwards
    /// makes that false, and issuing it anyway would take the monitor's handle count for the key
    /// down past the mapping just established -- unmapping the slot out from under whoever is
    /// holding the fresh handle. Dropping it is what keeps the count balanced: the clobbering
    /// insert in `RunComp::map_object` has already released the superseded one.
    pub fn cancel_pending_unmap(&mut self, map: &Mapping) {
        self.pending_unmaps.retain(|k| k != map);
    }

    /// Drop every entry idle longer than [IDLE_TTL].
    ///
    /// `queued` is ordered by release time, so this stops at the first entry still within its
    /// window rather than walking the whole queue.
    fn expire(&mut self, now: Instant) {
        loop {
            let Some((&seq, entry)) = self.queued.first_key_value() else {
                break;
            };
            if now.duration_since(entry.2) < IDLE_TTL {
                break;
            }
            // Unwrap-Ok: `first_key_value` just returned this key.
            let (map, handle, _) = self.queued.remove(&seq).unwrap();
            self.queued_at.remove(&map);
            tracing::debug!("expire {:?}", map);
            self.do_remove(&handle);
        }
    }

    /// Drop what has gone stale, for a caller with no other reason to touch the cache.
    ///
    /// Expiry otherwise happens on release, which is enough while a compartment is doing anything
    /// at all but never fires once it goes quiet -- which is exactly when a held mapping has the
    /// longest to cost. There is no background thread in this runtime to sweep from (the only
    /// spawned thread is the socket engine's, and only in compartments that use sockets), so this
    /// hangs off `twz_rt_gc`, the ABI's existing "give back what you can" entry point.
    pub fn sweep(&mut self) {
        self.expire(Instant::now());
    }

    /// Release a handle. Must only be called from runtime handle release (internal_refs == 0).
    pub fn release(&mut self, handle: &object_handle, flags: release_flags) {
        let map = ObjectMapKey::from_raw_handle(handle);
        tracing::debug!("release {:?}", map);
        if let Some(handle) = self.active.remove(&map) {
            if flags & RELEASE_NO_CACHE != 0 {
                self.do_remove(&handle);
                return;
            }
            let now = Instant::now();
            self.expire(now);
            // Still full after expiring: give up the least recently released, which is the
            // lowest sequence since the sequence only ever increases.
            if self.queued.len() >= QUEUE_LEN {
                // Unwrap-Ok: the length check says it is non-empty, and `first_key_value` then
                // returns the key `remove` is given.
                let seq = *self.queued.first_key_value().unwrap().0;
                let (oldmap, old, _) = self.queued.remove(&seq).unwrap();
                self.queued_at.remove(&oldmap);
                tracing::debug!("evict {:?}", oldmap);
                self.do_remove(&old);
            }
            tracing::debug!("queuing");
            let seq = self.next_seq;
            self.next_seq += 1;
            self.queued.insert(seq, (map, handle, now));
            self.queued_at.insert(map, seq);
        } else if self.queued_at.contains_key(&map) {
            // Already released and sitting in the cache. Reachable only via the resurrect path:
            // `cached` handed this handle out again after its count hit zero, and that holder has
            // now dropped it too, so the release runs a second time. The queued entry is the live
            // record -- unmapping here would tear down a mapping the cache still owns.
            tracing::debug!("release: already queued, leaving cached");
        } else {
            tracing::debug!("do_remove");
            self.do_remove(handle);
        }
    }

    /// Flush all items in the inactive queue.
    pub fn flush(&mut self) {
        let to_remove = core::mem::take(&mut self.queued);
        self.queued_at.clear();
        for (_, item) in to_remove {
            tracing::trace!("flush: remove: {}", item.0 .0);
            self.do_remove(&item.1);
        }
    }
}
