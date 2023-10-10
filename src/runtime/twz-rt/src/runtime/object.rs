use twizzler_runtime_api::ObjectRuntime;

use super::ReferenceRuntime;

impl ObjectRuntime for ReferenceRuntime {
    fn map_object(
        &self,
        id: twizzler_runtime_api::ObjID,
        flags: twizzler_runtime_api::MapFlags,
    ) -> Result<twizzler_runtime_api::ObjectHandle, twizzler_runtime_api::MapError> {
        todo!()
    }

    fn unmap_object(&self, handle: &twizzler_runtime_api::ObjectHandle) {
        todo!()
    }

    fn release_handle(&self, handle: &mut twizzler_runtime_api::ObjectHandle) {
        todo!()
    }
}
