use std::ffi::CString;

use twizzler_runtime_api::DebugRuntime;

use crate::monitor;

use super::ReferenceRuntime;

// Most of these implementations just delegate to asking the monitor for library
// information. In the future, when we properly have compartments, these will get
// more complicated.
impl DebugRuntime for ReferenceRuntime {
    fn get_library(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        monitor::get_monitor_actions().lookup_library_by_id(id)
    }

    fn get_exeid(&self) -> Option<twizzler_runtime_api::LibraryId> {
        monitor::get_monitor_actions().local_primary()
    }

    fn get_library_segment(
        &self,
        lib: &twizzler_runtime_api::Library,
        seg: usize,
    ) -> Option<twizzler_runtime_api::AddrRange> {
        monitor::get_monitor_actions().get_segment(lib.id, seg)
    }

    fn get_full_mapping(
        &self,
        lib: &twizzler_runtime_api::Library,
    ) -> Option<twizzler_runtime_api::ObjectHandle> {
        Some(lib.mapping.clone())
    }

    fn iterate_phdr(
        &self,
        f: &mut dyn FnMut(twizzler_runtime_api::DlPhdrInfo) -> core::ffi::c_int,
    ) -> core::ffi::c_int {
        let mut ret = 0;
        // Get the primary library for this compartment.
        let mut id = self.get_exeid();
        // Each library contains a field indicating the next library ID in this list.
        while let Some(library) = id.and_then(|id| self.get_library(id)) {
            if let Some(mut info) = library.dl_info {
                // Read the name. If it's too long, just give up for now.
                // TODO: improve this for longer names.
                let mut buf = [0; 256];
                let name = self
                    .get_library_name(&library, &mut buf)
                    .map(|len| {
                        let mut v = buf[0..len].to_vec();
                        // Null-terminate, since it needs to be a C string.
                        v.push(0);
                        CString::from_vec_with_nul(v).unwrap()
                    })
                    .unwrap_or_else(|| CString::new(b"???\0".to_vec()).unwrap());
                info.name = name.as_c_str().as_ptr() as *const u8;
                ret = f(info);
                // dl_iterate_phdr returns early if the callback returns non-zero.
                if ret != 0 {
                    return ret;
                }
            }
            id = library.next_id;
        }
        ret
    }

    fn get_library_name(
        &self,
        lib: &twizzler_runtime_api::Library,
        buf: &mut [u8],
    ) -> Option<usize> {
        monitor::get_monitor_actions().lookup_library_name(lib.id, buf)
    }
}
