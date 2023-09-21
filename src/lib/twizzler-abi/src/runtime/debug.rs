use twizzler_runtime_api::DebugRuntime;

use super::MinimalRuntime;

impl DebugRuntime for MinimalRuntime {
    fn get_library(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        None
    }

    fn get_exeid(&self) -> Option<twizzler_runtime_api::LibraryId> {
        None
    }

    fn get_library_segment(
        &self,
        lib: &twizzler_runtime_api::Library,
        seg: usize,
    ) -> Option<twizzler_runtime_api::AddrRange> {
        None
    }

    fn get_full_mapping(
        &self,
        lib: &twizzler_runtime_api::Library,
    ) -> Option<twizzler_runtime_api::ObjectHandle> {
        None
    }
}
