use std::{collections::BTreeMap, sync::Mutex};

use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_rt_abi::{
    bindings::{dl_phdr_info, loaded_image, loaded_image_id},
    object::MapFlags,
};

use super::ReferenceRuntime;

static LIBNAMES: Mutex<BTreeMap<String, &'static [u8]>> = Mutex::new(BTreeMap::new());

impl ReferenceRuntime {
    pub fn get_image_info(&self, id: loaded_image_id) -> Option<loaded_image> {
        let lib = monitor_api::CompartmentHandle::current()
            .libs()
            .nth(id as usize)?;
        let mut info = lib.info();
        let mut lib_names = LIBNAMES.lock().ok()?;
        if !lib_names.contains_key(&info.name) {
            let mut name_bytes = info.name.clone().into_bytes();
            name_bytes.push(0);
            lib_names.insert(info.name.clone(), name_bytes.leak());
        }
        let name_ptr = lib_names.get(&info.name)?.as_ptr();
        info.dl_info.name = name_ptr.cast();
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
