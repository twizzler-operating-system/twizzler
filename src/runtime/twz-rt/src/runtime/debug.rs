use tracing::{debug, trace};
use twizzler_runtime_api::{DebugRuntime, DlPhdrInfo, Library};

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

    fn iterate_phdr(
        &self,
        f: &mut dyn FnMut(twizzler_runtime_api::DlPhdrInfo) -> core::ffi::c_int,
    ) -> core::ffi::c_int {
        fn build_dl_info(lib: Library) -> Option<DlPhdrInfo> {
            lib.dl_info.clone()
        }

        debug!("got iterate phdr");
        let mut ret = 0;
        for id in 0..usize::MAX {
            let Some(lib) = self.get_library(twizzler_runtime_api::LibraryId(id)) else {
                break;
            };
            let Some(dl_info) = build_dl_info(lib) else {
                continue;
            };
            ret = f(dl_info);
            if ret != 0 {
                return ret;
            }
        }
        ret
    }
}
