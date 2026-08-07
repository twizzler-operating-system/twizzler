use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use miette::IntoDiagnostic;
use monitor_api::{MappedObjectAddrs, SpaceStats};
use twizzler_abi::{
    object::Protections,
    syscall::{
        sys_object_create, sys_object_ctrl, sys_object_map, sys_object_unmap, BackingType,
        CreateTieFlags, CreateTieSpec, DeleteFlags, LifetimeType, ObjectControlCmd, ObjectCreate,
        ObjectCreateFlags, UnmapFlags,
    },
};
use twizzler_rt_abi::{
    bindings::{object_source, object_tie},
    error::{ResourceError, TwzError},
    object::{MapFlags, ObjID},
};

use self::handle::MapHandleInner;

mod handle;
mod unmapper;

pub use handle::MapHandle;
pub use unmapper::Unmapper;

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// A mapping of an object and flags.
pub struct MapInfo {
    pub(crate) id: ObjID,
    pub(crate) flags: MapFlags,
}

#[derive(Default)]
/// An address space we can map objects into.
pub struct Space {
    maps: HashMap<MapInfo, MappedObject>,
}

struct MappedObject {
    addrs: MappedObjectAddrs,
    handle_count: usize,
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

extern "C-unwind" {
    fn __monitor_get_slot() -> isize;
    fn __monitor_get_slot_pair(one: *mut usize, two: *mut usize) -> bool;
    fn __monitor_release_pair(one: usize, two: usize);
    fn __monitor_release_slot(slot: usize);
}

impl Space {
    /// Get the stats.
    pub fn stat(&self) -> SpaceStats {
        SpaceStats {
            mapped: self.maps.len(),
            active: self.maps.values().filter(|m| m.handle_count > 0).count(),
        }
    }

    /// Map an object into the space.
    pub fn map<'a>(this: &Mutex<Self>, info: MapInfo) -> Result<MapHandle, TwzError> {
        // Can't use the entry API here because the closure may fail.
        let mut guard = this.lock().unwrap();
        let item = match guard.maps.get_mut(&info) {
            Some(item) => item,
            None => {
                // Not yet mapped, so allocate a slot and map it.
                let slot = unsafe { __monitor_get_slot() }
                    .try_into()
                    .ok()
                    .ok_or(ResourceError::OutOfResources)?;

                drop(guard);
                let res = sys_object_map(
                    None,
                    info.id,
                    slot,
                    mapflags_into_prot(info.flags),
                    info.flags.into(),
                );
                guard = this.lock().unwrap();
                let Ok(_) = res else {
                    unsafe {
                        __monitor_release_slot(slot);
                    }
                    return Err(res.unwrap_err());
                };

                // The lock was dropped across the map syscall, so another thread may have mapped
                // this same object meanwhile. Inserting over it would strand that thread's
                // handles on a slot the table no longer names, and lose their references: the
                // first handle to drop would take the count to zero and unmap the surviving
                // slot out from under the others. Keep whichever mapping got there first.
                if guard.maps.contains_key(&info) {
                    let _ = sys_object_unmap(None, slot, UnmapFlags::empty()).inspect_err(|e| {
                        tracing::warn!("failed to unmap redundant mapping of {:?}: {}", info, e)
                    });
                    unsafe {
                        __monitor_release_slot(slot);
                    }
                } else {
                    guard.maps.insert(
                        info,
                        MappedObject {
                            addrs: MappedObjectAddrs::new(slot),
                            handle_count: 0,
                        },
                    );
                }
                // Unwrap-Ok: present either way.
                guard.maps.get_mut(&info).unwrap()
            }
        };

        // New maps will be set to zero, so this is unconditional.
        item.handle_count += 1;
        Ok(Arc::new(MapHandleInner::new(info, item.addrs)))
    }

    /// Map a pair of objects into the space.
    pub fn map_pair(
        &mut self,
        info: MapInfo,
        info2: MapInfo,
    ) -> Result<(MapHandle, MapHandle), TwzError> {
        // Not yet mapped, so allocate a slot and map it.
        let mut one = 0;
        let mut two = 0;
        if !unsafe { __monitor_get_slot_pair(&mut one, &mut two) } {
            return Err(ResourceError::OutOfResources.into());
        }

        let res = sys_object_map(
            None,
            info.id,
            one,
            mapflags_into_prot(info.flags),
            twizzler_abi::syscall::MapFlags::empty(),
        );
        if res.is_err() {
            unsafe {
                __monitor_release_pair(one, two);
            }
            return Err(res.unwrap_err());
        };

        let res = sys_object_map(
            None,
            info2.id,
            two,
            mapflags_into_prot(info2.flags),
            twizzler_abi::syscall::MapFlags::empty(),
        );
        if res.is_err() {
            let _ = sys_object_unmap(None, one, UnmapFlags::empty())
                .inspect_err(|e| tracing::warn!("failed to unmap first in pair on error: {}", e));
            unsafe {
                __monitor_release_pair(one, two);
            }
            return Err(res.unwrap_err());
        };

        // Deliberately NOT recorded in `maps`. That table is keyed by MapInfo alone and holds one
        // slot per object, but a pair mapping is tied to *this* load: the text must sit adjacent
        // to this load's data object, so the same object gets a different pair per compartment
        // (dynlink's engine uses the source object itself as the text object, so every compartment
        // that loads a library pair-maps the same id). Inserting here would replace the previous
        // load's entry, orphaning its handles from the table and leaving one reference count for
        // two live mappings — after which the first handle to drop unmaps the *other* load's slot
        // while it is still relocating. These handles own their slots outright instead.
        Ok((
            Arc::new(MapHandleInner::new_exclusive(
                info,
                MappedObjectAddrs::new(one),
            )),
            Arc::new(MapHandleInner::new_exclusive(
                info2,
                MappedObjectAddrs::new(two),
            )),
        ))
    }

    /// Remove an object from the space. The actual unmapping syscall only happens once the returned
    /// value from this function is dropped.
    pub fn handle_drop(&mut self, info: MapInfo) -> Option<UnmapOnDrop> {
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

    /// Utility function for creating an object and mapping it, deleting it if the mapping fails.
    pub fn safe_create_and_map_object(
        this: &Mutex<Self>,
        spec: ObjectCreate,
        sources: &[object_source],
        ties: &[object_tie],
        map_flags: MapFlags,
    ) -> miette::Result<MapHandle> {
        let id = sys_object_create(spec, sources, ties).into_diagnostic()?;
        tracing::trace!(
            "created object {} for mapping: {:?} {:?}",
            id,
            spec,
            map_flags
        );

        match Space::map(
            this,
            MapInfo {
                id,
                flags: map_flags,
            },
        ) {
            Ok(mh) => Ok(mh),
            Err(me) => {
                if let Err(e) =
                    sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0)
                {
                    tracing::warn!("failed to delete object {} after map failure {}", e, me);
                }
                Err(me)
            }
        }
        .into_diagnostic()
    }

    pub fn safe_create_and_map_runtime_object(
        this: &Mutex<Self>,
        instance: ObjID,
        map_flags: MapFlags,
    ) -> miette::Result<MapHandle> {
        Space::safe_create_and_map_object(
            this,
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                Some(instance),
                ObjectCreateFlags::DELETE,
                Protections::all(),
            ),
            &[],
            &[CreateTieSpec::new(instance, CreateTieFlags::empty()).into()],
            map_flags,
        )
    }
}

/// Allows us to call handle_drop and do all the hard work in the caller, since
/// the caller probably had to hold a lock to call these functions.
pub(crate) struct UnmapOnDrop {
    slot: usize,
}

impl UnmapOnDrop {
    pub(crate) fn new(slot: usize) -> Self {
        Self { slot }
    }
}

impl Drop for UnmapOnDrop {
    fn drop(&mut self) {
        match sys_object_unmap(None, self.slot, UnmapFlags::empty()) {
            Ok(_) => unsafe {
                __monitor_release_slot(self.slot);
            },
            Err(e) => {
                tracing::warn!("failed to unmap slot {}: {}", self.slot, e);
            }
        }
    }
}

/// Map an object into the address space, without tracking it. This leaks the mapping, but is useful
/// for bootstrapping. See the object mapping gate comments for more details.
pub fn early_object_map(info: MapInfo) -> MappedObjectAddrs {
    let slot = unsafe { __monitor_get_slot() }.try_into().unwrap();

    twizzler_abi::klog_println!(
        "early_object_map: mapping object {} into slot {}",
        info.id,
        slot
    );
    sys_object_map(
        None,
        info.id,
        slot,
        mapflags_into_prot(info.flags),
        twizzler_abi::syscall::MapFlags::empty(),
    )
    .unwrap();

    MappedObjectAddrs::new(slot)
}
