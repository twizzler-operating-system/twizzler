use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::sys_object_map,
};
use twizzler_runtime_api::{MapError, MapFlags, ObjectHandle, ObjectRuntime};

use super::{slot::early_slot_alloc, ReferenceRuntime};

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
        let slot = early_slot_alloc().ok_or(MapError::OutOfResources)?;
        let _ = sys_object_map(
            None,
            ObjID::new(id),
            slot,
            prot,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .map_err(|_| MapError::InternalError)?;
        Ok(ObjectHandle {
            id,
            flags,
            start: (slot * MAX_SIZE) as *mut u8,
            meta: (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
        })
    }

    fn unmap_object(&self, _handle: &twizzler_runtime_api::ObjectHandle) {}

    fn release_handle(&self, _handle: &mut twizzler_runtime_api::ObjectHandle) {}
}
