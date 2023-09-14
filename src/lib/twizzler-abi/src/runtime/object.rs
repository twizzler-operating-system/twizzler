use twizzler_runtime_api::ObjectRuntime;

use super::MinimalRuntime;

mod handle;
pub use handle::*;

pub(crate) mod slot;

impl ObjectRuntime for MinimalRuntime {
    fn map_object(
        &self,
        id: twizzler_runtime_api::ObjID,
        flags: twizzler_runtime_api::MapFlags,
    ) -> Result<twizzler_runtime_api::ObjectHandle, twizzler_runtime_api::MapError> {
        todo!()
    }

    fn unmap_object(&self, handle: twizzler_runtime_api::ObjectHandle) {
        todo!()
    }
}
