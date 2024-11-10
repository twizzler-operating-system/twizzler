//! Null implementation of the debug runtime.

use super::{
    MinimalRuntime,
    phdrs::PHDR_INFO,
    load_elf::{ElfObject, PhdrType},
};
use crate::object::{InternalObject, ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE};

use twizzler_rt_abi::bindings::{loaded_image, object_handle, dl_phdr_info};

impl MinimalRuntime {
    pub fn get_image_info(&self) -> loaded_image {
        loaded_image {
           image_handle: object_handle {
               id: 0,
               runtime_info: core::ptr::null_mut(),
               start: core::ptr::null_mut(),
               meta: core::ptr::null_mut(),
               map_flags: 0,
               valid_len: (MAX_SIZE - NULLPAGE_SIZE * 2) as u32,
           }, 
           image_start: NULLPAGE_SIZE as *const core::ffi::c_void,
           image_len: MAX_SIZE - NULLPAGE_SIZE * 2,
           dl_info: dl_phdr_info {
               addr: 0,
               name: core::ptr::null_mut(),
               phdr: unsafe {PHDR_INFO}.map(|info| info.as_ptr()).unwrap_or(core::ptr::null_mut()).cast(),
               phnum: unsafe{PHDR_INFO}.map(|info| info.len()).unwrap_or(0) as u32,
               adds: 0,
               subs: 0,
               tls_modid: 0,
               tls_data: core::ptr::null_mut(),
           },
           id: 0, 
        }
    }
}
