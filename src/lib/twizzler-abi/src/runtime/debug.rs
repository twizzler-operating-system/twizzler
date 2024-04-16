//! Null implementation of the debug runtime.

use twizzler_runtime_api::{AddrRange, DebugRuntime, Library, LibraryId, MapFlags};

use crate::object::{InternalObject, ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE};

use super::{
    MinimalRuntime, __twz_get_runtime,
    load_elf::{ElfObject, PhdrType},
};

static mut EXEC_ID: ObjID = ObjID::new(0);

pub fn set_execid(id: ObjID) {
    unsafe { EXEC_ID = id }
}

fn get_execid() -> ObjID {
    unsafe { EXEC_ID }
}

impl DebugRuntime for MinimalRuntime {
    fn get_library(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        let mapping = __twz_get_runtime()
            .map_object(get_execid(), MapFlags::READ)
            .ok()?;
        Some(Library {
            range: AddrRange {
                start: mapping.start as usize + NULLPAGE_SIZE,
                len: MAX_SIZE - NULLPAGE_SIZE,
            },
            mapping,
            dl_info: None,
            next_id: None,
            id,
        })
    }

    fn get_exeid(&self) -> Option<twizzler_runtime_api::LibraryId> {
        Some(LibraryId(0))
    }

    fn get_library_segment(
        &self,
        lib: &twizzler_runtime_api::Library,
        seg: usize,
    ) -> Option<twizzler_runtime_api::AddrRange> {
        let exe = InternalObject::map(lib.mapping.id.into(), Protections::READ)?;
        let elf = ElfObject::from_obj(&exe)?;

        elf.phdrs()
            .filter(|p| p.phdr_type() == PhdrType::Load)
            .map(|p| twizzler_runtime_api::AddrRange {
                start: p.vaddr as usize,
                len: p.memsz as usize,
            })
            .nth(seg)
    }

    fn get_full_mapping(
        &self,
        lib: &twizzler_runtime_api::Library,
    ) -> Option<twizzler_runtime_api::ObjectHandle> {
        Some(lib.mapping.clone())
    }

    // The minimal runtime doesn't provide this, since we can get segment information in a simpler way for static binaries.
    fn iterate_phdr(
        &self,
        _f: &mut dyn FnMut(twizzler_runtime_api::DlPhdrInfo) -> core::ffi::c_int,
    ) -> core::ffi::c_int {
        0
    }
}
