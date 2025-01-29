use std::collections::BTreeMap;

use tracing::trace;
use twizzler_abi::object::MAX_SIZE;
use twizzler_rt_abi::bindings::object_handle;

use super::free_runtime_info;
use crate::runtime::object::ObjectMapKey;

type Mapping = super::ObjectMapKey;

const QUEUE_LEN: usize = 32;

#[derive(Default, Debug, Clone)]
pub struct HandleCache {
    active: BTreeMap<Mapping, object_handle>,
    queued: Vec<(Mapping, object_handle)>,
    slotmap: BTreeMap<usize, Mapping>,
}

// Safety: this is needed because of the raw pointers in object_handle, but that's okay here
// because those pointers are not used within the handle cache.
unsafe impl Send for HandleCache {}

fn do_unmap(handle: &object_handle) {
    let map = Mapping::from_raw_handle(handle);
    // No one else has a reference outside of the runtime, and we are being called by the handle
    // cache after it has bumped us from the queue. Thus, the internal refs had to be zero (to
    // be on the queue) and never incremented.
    free_runtime_info(handle.runtime_info.cast());
    monitor_api::monitor_rt_object_unmap(map.0, map.1).unwrap();
}

impl HandleCache {
    pub const fn new() -> Self {
        Self {
            active: BTreeMap::new(),
            queued: Vec::new(),
            slotmap: BTreeMap::new(),
        }
    }

    /// If map is present in either the active or the inactive lists, return a mutable reference to
    /// it. If the handle was inactive, move it to the active list.
    pub fn activate(&mut self, map: Mapping) -> Option<object_handle> {
        let idx = self.queued.iter().position(|item| item.0 == map);
        if let Some(idx) = idx {
            trace!("activate {:?} from queue pos {}", map, idx);
            let (_, handle) = self.queued.remove(idx);
            self.insert(handle);
        } else {
            trace!("activate {:?}", map);
        }

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

    fn do_remove(&mut self, item: &object_handle) {
        let slot = (item.start as usize) / MAX_SIZE;
        do_unmap(&item);
        self.slotmap.remove(&slot);
    }

    /// Release a handle. Must only be called from runtime handle release (internal_refs == 0).
    pub fn release(&mut self, handle: &object_handle) {
        let map = ObjectMapKey::from_raw_handle(handle);
        tracing::info!("release {:?}", map);
        if let Some(handle) = self.active.remove(&map) {
            // If queue is full, evict.
            if self.queued.len() >= QUEUE_LEN {
                let (oldmap, old) = self.queued.remove(0);
                trace!("evict {:?}", oldmap);
                self.do_remove(&old);
            }
            self.queued.push((map, handle));
        } else {
            self.do_remove(handle);
        }
    }

    /// Flush all items in the inactive queue.
    pub fn flush(&mut self) {
        let to_remove = self.queued.drain(..).collect::<Vec<_>>();
        for item in to_remove {
            tracing::info!("flush: remove: {}", item.0 .0);
            self.do_remove(&item.1);
        }
    }
}
