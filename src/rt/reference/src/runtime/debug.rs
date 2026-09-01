use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

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
        if let Some(image) = current.libs().nth(n) {
            return Some((None, image));
        }
        let Some(mut n) = n.checked_sub(current.info().ok()?.nr_libs) else {
            return None;
        };
        for dep in current.deps() {
            if let Some(image) = dep.libs().nth(n) {
                let name = dep.info().ok()?.name.clone();
                return Some((Some(name), image));
            }
            n = match n.checked_sub(dep.info().ok()?.nr_libs) {
                Some(rem) => rem,
                None => return None,
            };
        }
        None
    }

    pub fn get_image_info(&self, id: loaded_image_id) -> Option<loaded_image> {
        match self.image_info(id) {
            ImageLookup::Found(image) => Some(image),
            ImageLookup::Skip | ImageLookup::End => None,
        }
    }

    /// Look up one image index, distinguishing "past the end" from "exists but undescribable".
    ///
    /// Only [`Self::find_comp_dep_lib`] returning `None` means the end of the list; every other
    /// failure is one library, and iteration has to continue past it. See [`ImageLookup::Skip`].
    fn image_info(&self, id: loaded_image_id) -> ImageLookup {
        let Some((cn, lib)) = self.find_comp_dep_lib(id) else {
            return ImageLookup::End;
        };
        match self.build_image(id, cn, lib) {
            Some(image) => ImageLookup::Found(image),
            None => {
                // Once, not per unwind: this runs on every panic.
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Ordering::Relaxed) {
                    twizzler_abi::klog_println!(
                        "dl_iterate_phdr: image {} cannot be described by the monitor; skipping it \
                         (its frames will have no unwind info)",
                        id
                    );
                }
                ImageLookup::Skip
            }
        }
    }

    fn build_image(
        &self,
        id: loaded_image_id,
        cn: Option<String>,
        lib: LibraryHandle,
    ) -> Option<loaded_image> {
        // Fallible: this is the `dl_iterate_phdr` path, which the unwinder walks on every panic.
        // The unwrap that used to be here turned a library the monitor could not describe into a
        // panic while a panic was already in flight -- "thread panicked while processing panic",
        // with whatever rustc was actually reporting destroyed.
        let mut info = lib.try_info()?;
        tracing::trace!("get_image_info: {:?}", info);
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
        loop {
            match self.image_info(n) {
                ImageLookup::Found(image) => {
                    ret = f(image.dl_info);
                    if ret != 0 {
                        return ret;
                    }
                }
                ImageLookup::Skip => {}
                ImageLookup::End => return ret,
            }
            n += 1;
        }
    }
}

/// Result of describing one loaded image, for [`ReferenceRuntime::iterate_phdr`].
enum ImageLookup {
    Found(loaded_image),
    /// This index names a real library the monitor could not describe. Iteration must continue:
    /// the unwinder finds `.eh_frame` through `dl_iterate_phdr`, so stopping here costs every
    /// *later* library its FDEs too, and phase 1 then runs off the end of the stack -- reported as
    /// `fatal runtime error: failed to initiate panic, error 5` (`_URC_END_OF_STACK`), which
    /// aborts with the panic that was being raised never printed.
    Skip,
    /// Past the last library.
    End,
}

/*
#[allow(dead_code)]
const MAX_FRAMES: usize = 100;
#[allow(dead_code)]
pub fn backtrace(_symbolize: bool, entry_point: Option<backtracer_core::EntryPoint>) {
    let mut frame_nr = 0;
    let trace_callback = |frame: &backtracer_core::Frame| {
        let ip = frame.ip();

        //if !symbolize {
        twizzler_abi::klog_println!("{:4} - {:18p}", frame_nr, ip);
        //}
        /*else {
            // Resolve this instruction pointer to a symbol name
            let _ = backtracer_core::resolve(
                if let Some(ctx) = DEBUG_CTX.poll().map(|d| &d.ctx) {
                    Some(ctx)
                } else {
                    None
                },
                0,
                ip,
                |symbol| {
                    let name = symbol.name();
                    if let Some(addr) = symbol.addr() {
                        emerglogln!(
                            "{:4}: {:18p} - {}",
                            frame_nr,
                            addr,
                            if let Some(ref name) = name {
                                name
                            } else {
                                "??"
                            }
                        )
                    } else {
                        emerglogln!(
                            "{:4}:                 ?? - {}",
                            frame_nr,
                            if let Some(ref name) = name {
                                name
                            } else {
                                "??"
                            }
                        )
                    }
                    if let Some(filename) = symbol.filename() {
                        if let Some(linenr) = symbol.lineno() {
                            emerglogln!(
                                "                               at {}:{}",
                                filename,
                                linenr
                            );
                        }
                    }
                },
            );
        }
        */
        frame_nr += 1;

        if frame_nr > MAX_FRAMES {
            return false;
        }

        true // keep going to the next frame
    };

    if let Some(entry_point) = entry_point {
        backtracer_core::trace_from(entry_point, trace_callback);
    } else {
        backtracer_core::trace(trace_callback);
    }
}

*/
