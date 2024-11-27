use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_rt_abi::{
    bindings::{dl_phdr_info, loaded_image, loaded_image_id},
    object::MapFlags,
};

use super::ReferenceRuntime;

impl ReferenceRuntime {
    pub fn get_image_info(&self, id: loaded_image_id) -> Option<loaded_image> {
        let lib = monitor_api::CompartmentHandle::current()
            .libs()
            .nth(id as usize)?;
        let info = lib.info();
        let handle = self.map_object(info.objid, MapFlags::READ).ok()?;
        Some(loaded_image {
            image_start: unsafe { handle.start().add(NULLPAGE_SIZE).cast() },
            image_len: handle.valid_len(),
            image_handle: handle.into_raw(),
            dl_info: info.dl_info,
            id,
        })
    }

    pub fn iterate_phdr(
        &self,
        f: &mut dyn FnMut(dl_phdr_info) -> core::ffi::c_int,
    ) -> core::ffi::c_int {
        let mut n = 0;
        let mut ret = 0;
        while let Some(image) = self.get_image_info(n) {
            ret = f(image.dl_info);
            if ret != 0 {
                return ret;
            }
            n += 1;
        }
        ret
    }
}
