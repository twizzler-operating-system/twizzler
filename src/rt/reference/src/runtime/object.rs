use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, AtomicUsize},
};

use tracing::warn;
use twizzler_abi::{
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_map, ObjectMapError},
};
use twizzler_rt_abi::{
    bindings::object_handle,
    object::{MapError, MapFlags, ObjID, ObjectHandle},
};

use super::ReferenceRuntime;

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

#[repr(C)]
pub(crate) struct RuntimeHandleInfo {
    refs: AtomicU64,
}

pub(crate) fn new_runtime_info() -> *mut RuntimeHandleInfo {
    let rhi = Box::new(RuntimeHandleInfo {
        refs: AtomicU64::new(1),
    });
    Box::into_raw(rhi)
}

pub(crate) fn new_object_handle(id: ObjID, slot: usize, flags: MapFlags) -> ObjectHandle {
    unsafe {
        ObjectHandle::new(
            id,
            new_runtime_info().cast(),
            (slot * MAX_SIZE) as *mut _,
            (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut _,
            flags,
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }
}

fn map_sys_err(sys_err: ObjectMapError) -> MapError {
    // TODO (dbittman): in a future PR, I plan to cleanup all the error handling between the API and
    // ABI crates.
    match sys_err {
        ObjectMapError::Unknown => MapError::Other,
        ObjectMapError::ObjectNotFound => MapError::NoSuchObject,
        ObjectMapError::InvalidSlot => MapError::InvalidArgument,
        ObjectMapError::InvalidProtections => MapError::PermissionDenied,
        ObjectMapError::InvalidArgument => MapError::InvalidArgument,
    }
}

impl ReferenceRuntime {
    #[tracing::instrument(ret, skip(self), level = "trace")]
    pub fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle, MapError> {
        self.object_manager
            .lock()
            .unwrap()
            .map_object(ObjectMapKey(id.into(), flags))
    }

    #[tracing::instrument(skip(self), level = "trace")]
    pub fn release_handle(&self, handle: *mut object_handle) {
        self.object_manager.lock().unwrap().release(handle)
    }

    pub fn map_two_objects(
        &self,
        in_id_a: ObjID,
        in_flags_a: MapFlags,
        in_id_b: ObjID,
        in_flags_b: MapFlags,
    ) -> Result<(ObjectHandle, ObjectHandle), MapError> {
        let (slot_a, slot_b) = self.allocate_pair().ok_or(MapError::OutOfResources)?;

        let prot_a = mapflags_into_prot(in_flags_a);
        let prot_b = mapflags_into_prot(in_flags_b);

        sys_object_map(
            None,
            in_id_a,
            slot_a,
            prot_a,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .map_err(map_sys_err)?;

        sys_object_map(
            None,
            in_id_b,
            slot_b,
            prot_b,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .map_err(map_sys_err)?;

        Ok((
            new_object_handle(in_id_a, slot_a, in_flags_a),
            new_object_handle(in_id_b, slot_b, in_flags_b),
        ))
    }
}

/// A key for local (per-compartment) mappings of objects.
#[derive(PartialEq, PartialOrd, Ord, Eq, Hash, Copy, Clone)]
pub struct ObjectMapKey(pub ObjID, pub MapFlags);

/// A local slot for an object to be mapped, gotten from the monitor.
pub struct LocalSlot {
    number: usize,
    refs: AtomicUsize,
}

impl LocalSlot {
    pub fn new(number: usize) -> Self {
        Self {
            number,
            refs: AtomicUsize::new(0),
        }
    }
}

/// Per-compartment object management.
pub struct ObjectHandleManager {
    map: Option<HashMap<ObjectMapKey, LocalSlot>>,
}

impl ObjectHandleManager {
    pub const fn new() -> Self {
        Self { map: None }
    }

    fn map(&mut self) -> &mut HashMap<ObjectMapKey, LocalSlot> {
        if self.map.is_some() {
            // Unwrap-Ok: is_some above.
            return self.map.as_mut().unwrap();
        }
        self.map = Some(HashMap::new());
        // Unwrap-Ok: set above.
        self.map.as_mut().unwrap()
    }

    /// Map an object with this manager. Will call to monitor if needed.
    pub fn map_object(&mut self, key: ObjectMapKey) -> Result<ObjectHandle, MapError> {
        if !self.map().contains_key(&key) {
            self.map().insert(
                key,
                LocalSlot::new(
                    monitor_api::monitor_rt_object_map(key.0, key.1)
                        .unwrap()?
                        .slot,
                ),
            );
        }

        // Unwrap-Ok: we ensure key is present above.
        let entry = self.map().get(&key).unwrap();
        entry.refs.fetch_add(1, atomic::Ordering::SeqCst);

        Ok(new_object_handle(key.0, entry.number, key.1))
    }

    /// Release a handle. If all handles have been released, calls to monitor to unmap.
    pub fn release(&mut self, handle: *mut object_handle) {
        let handle = unsafe { handle.as_ref().unwrap() };
        let key = ObjectMapKey(
            ObjID::new(handle.id),
            MapFlags::from_bits_truncate(handle.map_flags),
        );
        if let Some(entry) = self.map().get(&key) {
            if entry.refs.fetch_sub(1, atomic::Ordering::SeqCst) == 1 {
                monitor_api::monitor_rt_object_unmap(key.0, key.1).unwrap();
                self.map().remove(&key);
            }
        }

        // Safety: we only create internal refs from Box.
        let _boxed = unsafe { Box::from_raw(handle.runtime_info) };
    }
}
