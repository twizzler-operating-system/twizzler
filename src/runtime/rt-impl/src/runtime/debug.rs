use std::{collections::BTreeMap, sync::Mutex};

use monitor_api::{CompartmentHandle, LibraryHandle};
use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_rt_abi::{
    bindings::{dl_phdr_info, loaded_image, loaded_image_id},
    object::MapFlags,
};

use super::ReferenceRuntime;

static LIBNAMES: Mutex<BTreeMap<String, &'static [u8]>> = Mutex::new(BTreeMap::new());

impl ReferenceRuntime {
    fn find_comp_dep_lib(&self, id: loaded_image_id) -> Option<(Option<String>, LibraryHandle)> {
        let n = id as usize;
        let current = CompartmentHandle::current();
        /*
        tracing::info!(
            "find comp dep lib: {}, current has {}",
            n,
            current.info().nr_libs
        );
        */
        if let Some(image) = current.libs().nth(n) {
            return Some((None, image));
        }
        let Some(mut n) = n.checked_sub(current.info().nr_libs) else {
            return None;
        };
        for dep in current.deps() {
            // tracing::info!("checking dep: {:?}", dep.info());
            if let Some(image) = dep.libs().nth(n) {
                let name = dep.info().name.clone();
                return Some((Some(name), image));
            }
            n = match n.checked_sub(dep.info().nr_libs) {
                Some(rem) => rem,
                None => return None,
            };
        }
        None
    }

    pub fn get_image_info(&self, id: loaded_image_id) -> Option<loaded_image> {
        let (cn, lib) = self.find_comp_dep_lib(id)?;
        let mut info = lib.info();
        tracing::info!("get_image_info: {:?}", info);
        let mut lib_names = LIBNAMES.lock().ok()?;
        let fullname = if let Some(cn) = cn {
            format!("{}::{}", cn, info.name)
        } else {
            info.name.clone()
        };
        if !lib_names.contains_key(&fullname) {
            let mut name_bytes = fullname.clone().into_bytes();
            name_bytes.push(0);
            lib_names.insert(fullname.clone(), name_bytes.leak());
        }
        let name_ptr = lib_names.get(&fullname)?.as_ptr();
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
