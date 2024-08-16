use std::{collections::HashMap, ptr::NonNull, sync::atomic::AtomicUsize};

use tracing::warn;
use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_map, ObjectMapError},
};
use twizzler_runtime_api::{
    MapError, MapFlags, ObjectHandle, ObjectRuntime, StartOrHandle, StartOrHandleRef,
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

pub(crate) fn new_object_handle(
    id: twizzler_runtime_api::ObjID,
    slot: usize,
    flags: MapFlags,
) -> ObjectHandle {
    ObjectHandle::new(
        NonNull::new(Box::into_raw(Box::default())).unwrap(),
        id,
        flags,
        (slot * MAX_SIZE) as *mut u8,
        (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
    )
}

fn map_sys_err(sys_err: ObjectMapError) -> twizzler_runtime_api::MapError {
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

impl ObjectRuntime for ReferenceRuntime {
    #[tracing::instrument(ret, skip(self), level = "trace")]
    fn map_object(
        &self,
        id: twizzler_runtime_api::ObjID,
        flags: twizzler_runtime_api::MapFlags,
    ) -> Result<twizzler_runtime_api::ObjectHandle, twizzler_runtime_api::MapError> {
        self.object_manager
            .lock()
            .unwrap()
            .map_object(ObjectMapKey(id.into(), flags))
    }

    #[tracing::instrument(skip(self), level = "trace")]
    fn release_handle(&self, handle: &mut twizzler_runtime_api::ObjectHandle) {
        self.object_manager.lock().unwrap().release(handle)
    }

    fn map_two_objects(
        &self,
        in_id_a: twizzler_runtime_api::ObjID,
        in_flags_a: MapFlags,
        in_id_b: twizzler_runtime_api::ObjID,
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

    fn resolve_fot_to_object_start(
        &self,
        handle: StartOrHandleRef<'_>,
        idx: usize,
        valid_len: usize,
    ) -> Result<StartOrHandle, twizzler_runtime_api::FotResolveError> {
        todo!()
    }

    fn add_fot_entry(&self, handle: &ObjectHandle) -> Option<(*mut u8, usize)> {
        todo!()
    }

    fn ptr_to_handle(&self, va: *const u8) -> Option<(ObjectHandle, usize)> {
        todo!()
    }

    fn ptr_to_object_start(&self, va: *const u8, valid_len: usize) -> Option<(*const u8, usize)> {
        todo!()
    }
}

/// A key for local (per-compartment) mappings of objects.
#[derive(PartialEq, PartialOrd, Ord, Eq, Hash, Copy, Clone)]
pub struct ObjectMapKey(pub twizzler_runtime_api::ObjID, pub MapFlags);

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
    pub fn release(&mut self, handle: &mut ObjectHandle) {
        let key = ObjectMapKey(handle.id.into(), handle.flags);
        if let Some(entry) = self.map().get(&key) {
            if entry.refs.fetch_sub(1, atomic::Ordering::SeqCst) == 1 {
                monitor_api::monitor_rt_object_unmap(entry.number, handle.id, handle.flags)
                    .unwrap();
                self.map().remove(&key);
            }
        }

        // Safety: we only create internal refs from Box.
        let _boxed = unsafe { Box::from_raw(handle.internal_refs.as_mut()) };
    }
}
