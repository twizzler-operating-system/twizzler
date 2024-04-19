use elf::segment::Elf64_Phdr;
use monitor_api::get_comp_config;
use twizzler_runtime_api::{AddrRange, DebugRuntime, Library, MapFlags};

use crate::preinit_println;

use super::{object::new_object_handle, ReferenceRuntime};

impl DebugRuntime for ReferenceRuntime {
    fn get_library(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        let info: monitor_api::LibraryInfo =
            monitor_api::monitor_rt_get_library_info(id).unwrap()?;
        let handle = new_object_handle(info.objid, info.slot, MapFlags::READ);
        Some(Library {
            range: info.range,
            dl_info: Some(info.dl_info),
            id,
            mapping: handle,
            next_id: info.next_id,
        })
    }

    fn get_exeid(&self) -> Option<twizzler_runtime_api::LibraryId> {
        // For now, this will always be the third library, after runtime and libstd.
        // TODO (dbittman): once the monitor refactor is complete, this API will be fixed
        // to be more dynamic.
        Some(twizzler_runtime_api::LibraryId(3))
    }

    fn get_library_segment(
        &self,
        lib: &twizzler_runtime_api::Library,
        seg: usize,
    ) -> Option<twizzler_runtime_api::AddrRange> {
        const PT_LOAD: u32 = 1;
        let slice = unsafe {
            core::slice::from_raw_parts(
                lib.dl_info?.phdr_start as *const Elf64_Phdr,
                lib.dl_info?.phdr_num as usize,
            )
        };
        let phdr = slice.iter().filter(|p| p.p_type == PT_LOAD).nth(seg)?;
        Some(AddrRange {
            start: lib.dl_info?.addr + phdr.p_vaddr as usize,
            len: phdr.p_memsz as usize,
        })
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
            if let Some(info) = library.dl_info {
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
}
