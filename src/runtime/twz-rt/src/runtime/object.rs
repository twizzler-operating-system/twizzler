use std::ptr::NonNull;

use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_map, sys_object_unmap, UnmapFlags},
};
use twizzler_runtime_api::{InternalHandleRefs, MapError, MapFlags, ObjectHandle, ObjectRuntime};

use super::ReferenceRuntime;

// TODO: implement an object cache

impl ObjectRuntime for ReferenceRuntime {
    fn map_object(
        &self,
        id: twizzler_runtime_api::ObjID,
        flags: twizzler_runtime_api::MapFlags,
    ) -> Result<twizzler_runtime_api::ObjectHandle, twizzler_runtime_api::MapError> {
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
        let slot = self.allocate_slot().ok_or(MapError::OutOfResources)?;
        let _ = sys_object_map(
            None,
            ObjID::new(id),
            slot,
            prot,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .map_err(|_| MapError::InternalError)?;
        Ok(ObjectHandle::new(
            NonNull::new(Box::into_raw(Box::new(InternalHandleRefs::default()))).unwrap(),
            id,
            flags,
            (slot * MAX_SIZE) as *mut u8,
            (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
        ))
    }

    fn release_handle(&self, handle: &mut twizzler_runtime_api::ObjectHandle) {
        let slot = (handle.start as usize) / MAX_SIZE;

        if sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok() {
            self.release_slot(slot);
        }
    }
}
