use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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

/// Cache of the `dl_phdr_info` list `iterate_phdr` emits.
///
/// The unwinder calls `dl_iterate_phdr` on every unwind (every panic, every backtrace capture),
/// and each call otherwise makes a cross-compartment monitor gate call per library -- open the
/// handle, read its info, drop it -- times every library in this compartment and its deps. The
/// list is static except when a library is loaded or unloaded, which in this runtime only happens
/// through `__dlapi_open`/`__dlapi_close`; those bump [`PHDR_GEN`], which invalidates the cache.
/// The cached `dl_phdr_info` pointers stay valid because a loaded library keeps its fixed slot
/// address for the compartment's life, and the name points into the permanent [`LIBNAMES`] arena.
struct PhdrEntry(dl_phdr_info);
// SAFETY: the raw pointers inside are into permanently-loaded library images and the LIBNAMES
// arena; they outlive the cache and are only read, never used to alias mutable state.
unsafe impl Send for PhdrEntry {}

struct PhdrCache {
    gen: u64,
    entries: Vec<PhdrEntry>,
}

static PHDR_CACHE: Mutex<Option<PhdrCache>> = Mutex::new(None);
static PHDR_GEN: AtomicU64 = AtomicU64::new(0);

/// Invalidate the `dl_iterate_phdr` cache. Called after a library is loaded or unloaded, since
/// either changes the set of images the unwinder must see.
pub(crate) fn invalidate_phdr_cache() {
    PHDR_GEN.fetch_add(1, Ordering::Release);
}

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
        let fullname = match cn {
            Some(cn) => format!("{}::{}", cn, info.name),
            None => info.name.clone(),
        };
        info.dl_info.name = Self::intern_name(&fullname)?.cast();
        let handle = self.map_object(info.objid, MapFlags::READ).ok()?;
        Some(loaded_image {
            image_start: unsafe { handle.start().add(NULLPAGE_SIZE).cast() },
            image_len: handle.valid_len(),
            image_handle: handle.into_raw(),
            dl_info: info.dl_info,
            id,
        })
    }

    /// Intern a library name into the permanent [`LIBNAMES`] arena and return a pointer to its
    /// NUL-terminated bytes. `dl_phdr_info::name` must outlive every handle, so it cannot point at
    /// the transient `LibraryInfo` buffer `try_info` fills.
    fn intern_name(fullname: &str) -> Option<*const u8> {
        let mut lib_names = LIBNAMES.lock().ok()?;
        if !lib_names.contains_key(fullname) {
            let mut name_bytes = fullname.as_bytes().to_vec();
            name_bytes.push(0);
            lib_names.insert(fullname.to_string(), name_bytes.leak());
        }
        Some(lib_names.get(fullname)?.as_ptr())
    }

    /// The `dl_phdr_info` for one library, name interned. Unlike [`Self::build_image`] this maps
    /// nothing: `iterate_phdr` only ever reads `dl_info`, so mapping the image (as `build_image`
    /// does for `get_image_info`) is a wasted object map and handle per library per unwind.
    fn build_dl_info(&self, cn: Option<&str>, lib: &LibraryHandle) -> Option<dl_phdr_info> {
        // Fallible for the same reason build_image is: this runs on every unwind, so a library the
        // monitor cannot describe must be skipped, not panicked on, or a panic-in-panic aborts
        // with the original message lost.
        let mut info = lib.try_info()?;
        let fullname = match cn {
            Some(cn) => format!("{}::{}", cn, info.name),
            None => info.name.clone(),
        };
        info.dl_info.name = Self::intern_name(&fullname)?.cast();
        Some(info.dl_info)
    }

    /// Build the full `dl_phdr_info` list, one gate round per library. The slow path behind the
    /// cache in [`Self::iterate_phdr`]; a library the monitor cannot describe is skipped (one
    /// missing set of FDEs) rather than terminating the list, which would cost every later library
    /// its FDEs too -- see [`ImageLookup::Skip`].
    fn collect_phdrs(&self) -> Vec<PhdrEntry> {
        let mut out = Vec::new();
        let current = CompartmentHandle::current();
        for lib in current.libs() {
            if let Some(info) = self.build_dl_info(None, &lib) {
                out.push(PhdrEntry(info));
            }
        }
        for dep in current.deps() {
            let name = dep.info().ok().map(|i| i.name);
            for lib in dep.libs() {
                if let Some(info) = self.build_dl_info(name.as_deref(), &lib) {
                    out.push(PhdrEntry(info));
                }
            }
        }
        out
    }

    pub fn iterate_phdr(
        &self,
        f: &mut dyn FnMut(dl_phdr_info) -> core::ffi::c_int,
    ) -> core::ffi::c_int {
        // The unwinder calls this on every unwind, and rebuilding the list is O(libs) monitor gate
        // calls (a security-context switch each way, per library). The list only changes when a
        // library is loaded or unloaded, tracked by PHDR_GEN, so cache it and rebuild only on a
        // generation change. try_lock, not lock: a live rebuild could re-enter this (e.g. an
        // allocation hook that unwinds), and blocking on our own held lock would deadlock -- fall
        // back to an uncached walk instead.
        let cur_gen = PHDR_GEN.load(Ordering::Acquire);
        let Ok(mut guard) = PHDR_CACHE.try_lock() else {
            let mut ret = 0;
            let current = CompartmentHandle::current();
            for lib in current.libs() {
                if let Some(info) = self.build_dl_info(None, &lib) {
                    ret = f(info);
                    if ret != 0 {
                        return ret;
                    }
                }
            }
            for dep in current.deps() {
                let name = dep.info().ok().map(|i| i.name);
                for lib in dep.libs() {
                    if let Some(info) = self.build_dl_info(name.as_deref(), &lib) {
                        ret = f(info);
                        if ret != 0 {
                            return ret;
                        }
                    }
                }
            }
            return ret;
        };

        if guard.as_ref().map_or(true, |c| c.gen != cur_gen) {
            *guard = Some(PhdrCache {
                gen: cur_gen,
                entries: self.collect_phdrs(),
            });
        }

        let mut ret = 0;
        for entry in &guard.as_ref().unwrap().entries {
            ret = f(entry.0);
            if ret != 0 {
                break;
            }
        }
        ret
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
