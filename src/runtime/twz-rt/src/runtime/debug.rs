use std::ffi::CString;

use twizzler_runtime_api::DebugRuntime;

use crate::monitor;

use super::ReferenceRuntime;

// TODO: hook into dynlink for this stuff
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
        let mut id = self.get_exeid();
        while let Some(library) = id.and_then(|id| self.get_library(id)) {
            if let Some(mut info) = library.dl_info {
                let mut buf = [0; 256];
                let name = self
                    .get_library_name(&library, &mut buf)
                    .map(|len| {
                        let mut v = buf[0..len].to_vec();
                        v.push(0);
                        CString::from_vec_with_nul(v).unwrap()
                    })
                    .unwrap_or_else(|_| CString::new(vec![b'?', b'?', b'?', b'\0']).unwrap());
                info.name = name.as_c_str().as_ptr() as *const u8;
                ret = f(info);
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
    ) -> Result<usize, ()> {
        monitor::get_monitor_actions().lookup_library_name(lib.id, buf)
    }
}
