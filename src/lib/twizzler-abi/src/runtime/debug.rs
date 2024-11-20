//! Null implementation of the debug runtime.

use twizzler_rt_abi::bindings::{dl_phdr_info, loaded_image, loaded_image_id, object_handle};

use super::{
    load_elf::{ElfObject, PhdrType},
    phdrs::PHDR_INFO,
    MinimalRuntime,
};
use crate::object::{InternalObject, ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE};

const NAME: &'static core::ffi::CStr = c"<main>";
impl MinimalRuntime {
    pub fn get_image_info(&self, id: loaded_image_id) -> Option<loaded_image> {
        if id != 0 {
            return None;
        }
        Some(loaded_image {
            image_handle: object_handle {
                id: 0,
                runtime_info: core::ptr::null_mut(),
                start: core::ptr::null_mut(),
                meta: core::ptr::null_mut(),
                map_flags: 0,
                valid_len: (MAX_SIZE - NULLPAGE_SIZE * 2) as u32,
            },
            image_start: ((MAX_SIZE * crate::slot::RESERVED_IMAGE) + NULLPAGE_SIZE)
                as *const core::ffi::c_void,
            image_len: MAX_SIZE - NULLPAGE_SIZE * 2,
            dl_info: dl_phdr_info {
                addr: 0,
                name: NAME.as_ptr(),
                phdr: unsafe { PHDR_INFO }
                    .map(|info| info.as_ptr())
                    .unwrap_or(core::ptr::null_mut())
                    .cast(),
                phnum: unsafe { PHDR_INFO }.map(|info| info.len()).unwrap_or(0) as u32,
                adds: 0,
                subs: 0,
                tls_modid: 0,
                tls_data: core::ptr::null_mut(),
            },
            id: 0,
        })
    }

    pub fn iterate_phdr(
        &self,
        f: &mut dyn FnMut(dl_phdr_info) -> core::ffi::c_int,
    ) -> core::ffi::c_int {
        let image = self.get_image_info(0).unwrap();
        f(image.dl_info)
    }
}
