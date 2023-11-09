use tracing::{debug, trace};
use twizzler_runtime_api::DebugRuntime;

use crate::monitor;

use super::ReferenceRuntime;

// TODO: hook into dynlink for this stuff

impl DebugRuntime for ReferenceRuntime {
    fn get_library(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        debug!("get_library: {}", id.0);
        monitor::get_monitor_actions().lookup_library_by_id(id)
    }

    fn get_exeid(&self) -> Option<twizzler_runtime_api::LibraryId> {
        debug!("get_execid");
        None
    }

    fn get_library_segment(
        &self,
        lib: &twizzler_runtime_api::Library,
        seg: usize,
    ) -> Option<twizzler_runtime_api::AddrRange> {
        debug!("get lib seg: {:x} {}", lib.mapping.id, seg);
        None
    }

    fn get_full_mapping(
        &self,
        lib: &twizzler_runtime_api::Library,
    ) -> Option<twizzler_runtime_api::ObjectHandle> {
        debug!("get full mapping: {:x}", lib.mapping.id);
        None
    }
}
