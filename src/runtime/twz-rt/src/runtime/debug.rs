use elf::segment::Elf64_Phdr;
use monitor_api::get_comp_config;
use twizzler_runtime_api::{AddrRange, DebugRuntime, Library, MapFlags};

use super::{object::new_object_handle, ReferenceRuntime};
use crate::preinit_println;

impl DebugRuntime for ReferenceRuntime {
    #[tracing::instrument(skip(self))]
    fn get_library(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        tracing::info!("A");
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

    #[tracing::instrument(skip(self))]
    fn get_exeid(&self) -> Option<twizzler_runtime_api::LibraryId> {
        get_comp_config().root_library_id
    }

    #[tracing::instrument(skip_all)]
    fn get_library_segment(
        &self,
        lib: &twizzler_runtime_api::Library,
        seg: usize,
    ) -> Option<twizzler_runtime_api::AddrRange> {
        tracing::info!("A");
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

    #[tracing::instrument(skip_all)]
    fn get_full_mapping(
        &self,
        lib: &twizzler_runtime_api::Library,
    ) -> Option<twizzler_runtime_api::ObjectHandle> {
        tracing::info!("A");
        Some(lib.mapping.clone())
    }

    #[tracing::instrument(skip_all)]
    fn iterate_phdr(
        &self,
        f: &mut dyn FnMut(twizzler_runtime_api::DlPhdrInfo) -> core::ffi::c_int,
    ) -> core::ffi::c_int {
        tracing::info!("A");
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
