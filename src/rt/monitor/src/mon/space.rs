use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use miette::IntoDiagnostic;
use monitor_api::MappedObjectAddrs;
use twizzler_abi::{
    object::Protections,
    syscall::{
        sys_object_create, sys_object_ctrl, sys_object_map, sys_object_unmap, BackingType,
        CreateTieFlags, CreateTieSpec, DeleteFlags, LifetimeType, ObjectControlCmd, ObjectCreate,
        ObjectCreateFlags, ObjectSource, UnmapFlags,
    },
};
use twizzler_rt_abi::{
    error::{ResourceError, TwzError},
    object::{MapFlags, ObjID},
};

use self::handle::MapHandleInner;
use crate::gates::SpaceStats;

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

                let map = MappedObject {
                    addrs: MappedObjectAddrs::new(slot),
                    handle_count: 0,
                };
                guard.maps.insert(info, map);
                // Unwrap-Ok: just inserted.
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

        let map = MappedObject {
            addrs: MappedObjectAddrs::new(one),
            handle_count: 0,
        };
        let map2 = MappedObject {
            addrs: MappedObjectAddrs::new(two),
            handle_count: 0,
        };
        self.maps.insert(info, map);
        self.maps.insert(info2, map2);
        // Unwrap-Ok: just inserted.
        let item = self.maps.get_mut(&info).unwrap();
        item.handle_count += 1;
        let addrs = item.addrs;
        let item2 = self.maps.get_mut(&info2).unwrap();
        item2.handle_count += 1;
        let addrs2 = item2.addrs;
        Ok((
            Arc::new(MapHandleInner::new(info, addrs)),
            Arc::new(MapHandleInner::new(info2, addrs2)),
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
        tracing::debug!("drop: {:?}: handle count: {}", info, item.handle_count);
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
    pub(crate) fn safe_create_and_map_object(
        this: &Mutex<Self>,
        spec: ObjectCreate,
        sources: &[ObjectSource],
        ties: &[CreateTieSpec],
        map_flags: MapFlags,
    ) -> miette::Result<MapHandle> {
        let id = sys_object_create(spec, sources, ties).into_diagnostic()?;

        match Space::map(
            this,
            MapInfo {
                id,
                flags: map_flags,
            },
        ) {
            Ok(mh) => Ok(mh),
            Err(me) => {
                if let Err(e) = sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()))
                {
                    tracing::warn!("failed to delete object {} after map failure {}", e, me);
                }
                Err(me)
            }
        }
        .into_diagnostic()
    }

    pub(crate) fn safe_create_and_map_runtime_object(
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
            &[CreateTieSpec::new(instance, CreateTieFlags::empty())],
            map_flags,
        )
    }
}

/// Allows us to call handle_drop and do all the hard work in the caller, since
/// the caller probably had to hold a lock to call these functions.
pub(crate) struct UnmapOnDrop {
    slot: usize,
}

impl Drop for UnmapOnDrop {
    fn drop(&mut self) {
        match sys_object_unmap(None, self.slot, UnmapFlags::empty()) {
            Ok(_) => unsafe {
                __monitor_release_slot(self.slot);
            },
            Err(_e) => {
                // TODO: once the kernel-side works properly, uncomment this.
                //tracing::warn!("failed to unmap slot {}: {}", self.slot, e);
            }
        }
    }
}

/// Map an object into the address space, without tracking it. This leaks the mapping, but is useful
/// for bootstrapping. See the object mapping gate comments for more details.
pub fn early_object_map(info: MapInfo) -> MappedObjectAddrs {
    let slot = unsafe { __monitor_get_slot() }.try_into().unwrap();

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
