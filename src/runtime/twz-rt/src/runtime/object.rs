use std::ptr::NonNull;

use tracing::warn;
use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_map, sys_object_unmap, ObjectMapError, UnmapFlags},
};
use twizzler_runtime_api::{MapError, MapFlags, ObjectHandle, ObjectRuntime};

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
    // TODO (dbittman): in a future PR, I plan to cleanup all the error handling between the API and ABI crates.
    match sys_err {
        ObjectMapError::Unknown => MapError::Other,
        ObjectMapError::ObjectNotFound => MapError::NoSuchObject,
        ObjectMapError::InvalidSlot => MapError::InvalidArgument,
        ObjectMapError::InvalidProtections => MapError::PermissionDenied,
        ObjectMapError::InvalidArgument => MapError::InvalidArgument,
    }
}

// TODO: implement an object cache

impl ObjectRuntime for ReferenceRuntime {
    #[tracing::instrument(ret, skip(self), level = "trace")]
    fn map_object(
        &self,
        id: twizzler_runtime_api::ObjID,
        flags: twizzler_runtime_api::MapFlags,
    ) -> Result<twizzler_runtime_api::ObjectHandle, twizzler_runtime_api::MapError> {
        let slot = self.allocate_slot().ok_or(MapError::OutOfResources)?;
        sys_object_map(
            None,
            ObjID::new(id),
            slot,
            mapflags_into_prot(flags),
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .map_err(|_| MapError::InternalError)?;
        Ok(new_object_handle(id, slot, flags))
    }

    #[tracing::instrument(skip(self), level = "trace")]
    fn release_handle(&self, handle: &mut twizzler_runtime_api::ObjectHandle) {
        let slot = (handle.start as usize) / MAX_SIZE;

        if sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok() {
            self.release_slot(slot);
        } else {
            warn!("failed to unmap slot {}", slot);
        }

        // Safety: we only create internal refs from Box.
        let _boxed = unsafe { Box::from_raw(handle.internal_refs.as_mut()) };
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
            ObjID::new(in_id_a),
            slot_a,
            prot_a,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .map_err(map_sys_err)?;

        sys_object_map(
            None,
            ObjID::new(in_id_b),
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
