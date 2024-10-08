use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString},
    sync::Mutex,
};

use elf::segment::Elf64_Phdr;
use twizzler_runtime_api::{AddrRange, DebugRuntime, Library, MapFlags};

use super::{object::new_object_handle, ReferenceRuntime};

static LIBNAMES: Mutex<BTreeMap<String, &'static [u8]>> = Mutex::new(BTreeMap::new());

impl DebugRuntime for ReferenceRuntime {
    fn get_library(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        let lib = monitor_api::CompartmentHandle::current().libs().nth(id.0)?;
        let mut info = lib.info();
        let mut lib_names = LIBNAMES.lock().ok()?;
        if !lib_names.contains_key(&info.name) {
            let mut name_bytes = info.name.clone().into_bytes();
            name_bytes.push(0);
            lib_names.insert(info.name.clone(), name_bytes.leak());
        }
        let name_ptr = lib_names.get(&info.name)?.as_ptr();
        info.dl_info.name = name_ptr;
        let handle = new_object_handle(info.objid, info.slot, MapFlags::READ);
        Some(Library {
            range: info.range,
            dl_info: Some(info.dl_info),
            id,
            mapping: handle,
        })
    }

    fn get_exeid(&self) -> Option<twizzler_runtime_api::LibraryId> {
        // root ID is always 0
        Some(twizzler_runtime_api::LibraryId(0))
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
        let mut id = self.get_exeid().unwrap().0;
        // Each library contains a field indicating the next library ID in this list.
        while let Some(library) = self.get_library(twizzler_runtime_api::LibraryId(id)) {
            if let Some(info) = library.dl_info {
                ret = f(info);
                // dl_iterate_phdr returns early if the callback returns non-zero.
                if ret != 0 {
                    return ret;
                }
            }
            id += 1;
        }
        ret
    }

    fn next_library_id(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::LibraryId> {
        Some(twizzler_runtime_api::LibraryId(id.0 + 1))
    }
}
