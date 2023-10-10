use twizzler_runtime_api::DebugRuntime;

use super::ReferenceRuntime;

impl DebugRuntime for ReferenceRuntime {
    fn get_library(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        todo!()
    }

    fn get_exeid(&self) -> Option<twizzler_runtime_api::LibraryId> {
        todo!()
    }

    fn get_library_segment(
        &self,
        lib: &twizzler_runtime_api::Library,
        seg: usize,
    ) -> Option<twizzler_runtime_api::AddrRange> {
        todo!()
    }

    fn get_full_mapping(
        &self,
        lib: &twizzler_runtime_api::Library,
    ) -> Option<twizzler_runtime_api::ObjectHandle> {
        todo!()
    }
}
