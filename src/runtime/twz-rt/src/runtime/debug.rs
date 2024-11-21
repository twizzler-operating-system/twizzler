use elf::segment::Elf64_Phdr;
use twizzler_rt_abi::{
    bindings::TWZ_RT_EXEID,
    debug::{LoadedImage, LoadedImageId},
    object::MapFlags,
};

use super::{object::new_object_handle, ReferenceRuntime};

impl ReferenceRuntime {
    fn get_library(&self, id: LoadedImageId) -> Option<LoadedImage> {
        let lib = monitor_api::CompartmentHandle::current()
            .libs()
            .nth(id as usize)?;
        let info = lib.info();
        let handle = new_object_handle(info.objid, info.slot, MapFlags::READ);
        /*
        Some(Library {
            range: info.range,
            dl_info: Some(info.dl_info),
            id,
            mapping: handle,
        })
        */
        todo!()
    }

    fn iterate_phdr(
        &self,
        f: &mut dyn FnMut(twizzler_rt_abi::debug::DlPhdrInfo) -> core::ffi::c_int,
    ) -> core::ffi::c_int {
        todo!()
    }
}
