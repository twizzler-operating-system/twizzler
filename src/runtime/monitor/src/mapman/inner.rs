//! Map Manager Inner
//!
//! Manages inner mappings, tracking handle count for monitor internal mapping and tracking.

use std::{collections::HashMap, sync::Arc};

use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_map, sys_object_unmap, UnmapFlags},
};
use twizzler_runtime_api::{MapError, MapFlags};

use super::{
    handle::{MapHandle, MapHandleInner},
    info::MapInfo,
};
use twizzler_abi::object::Protections;

pub(super) struct MMInner {
    maps: HashMap<MapInfo, MappedObject>,
}

struct MappedObject {
    addrs: MappedObjectAddrs,
    handle_count: usize,
}

/// Contains raw mapping addresses, for use when translating to object handles for the runtime.
#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub struct MappedObjectAddrs {
    slot: usize,
    start: usize,
    meta: usize,
}

fn mapflags_into_prot(flags: MapFlags) -> Protections {
    let mut prot = Protections::empty();
    if flags.contains(MapFlags::READ) {
        prot.insert(Protections::READ);
    }
    if flags.contains(MapFlags::WRITE) {
        prot.insert(Protections::WRITE);
    }
    if flags.contains(MapFlags::EXEC) {
        prot.insert(Protections::EXEC);
    }
    prot
}

impl MMInner {
    pub(super) fn new() -> Self {
        Self {
            maps: HashMap::new(),
        }
    }

    /// Map an object, returning a handle. On drop, that handle will signal the MapMan, which
    /// tracks active handles. Once all handles are dropped, the object will be unmapped.
    pub fn map(&mut self, info: MapInfo) -> Result<MapHandle, MapError> {
        // Can't use the entry API here because the closure may fail.
        let item = match self.maps.get_mut(&info) {
            Some(item) => item,
            None => {
                // Not yet mapped, so allocate a slot and map it.
                let slot = twz_rt::OUR_RUNTIME
                    .allocate_slot()
                    .ok_or(MapError::OutOfResources)?;

                let Ok(_) = sys_object_map(
                    None,
                    info.id,
                    slot,
                    mapflags_into_prot(info.flags),
                    twizzler_abi::syscall::MapFlags::empty(),
                ) else {
                    twz_rt::OUR_RUNTIME.release_slot(slot);
                    return Err(MapError::InternalError);
                };

                let map = MappedObject {
                    addrs: MappedObjectAddrs {
                        start: slot * MAX_SIZE,
                        meta: (slot + 1) * MAX_SIZE - NULLPAGE_SIZE,
                        slot,
                    },
                    handle_count: 0,
                };
                self.maps.insert(info, map);
                // Unwrap-Ok: just inserted.
                self.maps.get_mut(&info).unwrap()
            }
        };

        // New maps will be set to zero, so this is unconditional.
        item.handle_count += 1;
        Ok(Arc::new(MapHandleInner::new(info, item.addrs)))
    }

    pub(super) fn handle_drop(&mut self, info: MapInfo) -> Option<UnmapOnDrop> {
        // Missing maps in unmap should be ignored.
        let Some(item) = self.maps.get_mut(&info) else {
            tracing::warn!("unmap called for missing object {:?}", info);
            return None;
        };
        if item.handle_count == 0 {
            tracing::error!("unmap called for unmapped object {:?}", info);
            return None;
        }

        // Decrement and maybe actually unmap.
        item.handle_count -= 1;
        if item.handle_count == 0 {
            let slot = item.addrs.slot;
            self.maps.remove(&info);
            Some(UnmapOnDrop { slot })
        } else {
            None
        }
    }
}

// Allows us to call handle_drop and do all the hard work in the caller, since
// the caller probably had to hold a lock to call handle_drop in MMInner.
pub(crate) struct UnmapOnDrop {
    slot: usize,
}

impl Drop for UnmapOnDrop {
    fn drop(&mut self) {
        if sys_object_unmap(None, self.slot, UnmapFlags::empty()).is_ok() {
            twz_rt::OUR_RUNTIME.release_slot(self.slot);
        } else {
            tracing::warn!("failed to unmap slot {}", self.slot);
        }
    }
}
