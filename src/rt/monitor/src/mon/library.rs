use dynlink::{
    engines::LoadCtx,
    library::{AllowedGates, LibraryId, UnloadedLibrary},
    symbol::LookupFlags,
};
use happylock::ThreadKey;
use monitor_api::LibraryInfoRaw;
use secgate::util::Descriptor;
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::{
    bindings::{ctor_set, link_map},
    debug::LinkMap,
    error::{ArgumentError, GenericError, ResourceError, TwzError},
    object::ObjID,
};

use super::Monitor;

/// A handle to a library.
pub struct LibraryHandle {
    comp: ObjID,
    id: LibraryId,
}

impl Monitor {
    /// Get LibraryInfo for a given library handle. Note that this will write to the
    /// compartment-thread's simple buffer.
    pub fn get_library_info(
        &self,
        instance: ObjID,
        thread: ObjID,
        desc: Descriptor,
    ) -> Result<LibraryInfoRaw, TwzError> {
        let (_, ref mut comps, ref dynlink, ref libhandles, _) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let handle = libhandles
            .lookup(instance, desc)
            .ok_or(ArgumentError::InvalidArgument)?;
        // TODO: dynlink err map
        let lib = dynlink
            .get_library(handle.id)
            .map_err(|_| GenericError::Internal)?;
        // write the library name to the per-thread simple buffer
        let pt = comps.get_mut(instance)?.get_per_thread(thread);
        let name_len = pt.write_bytes(lib.name.as_bytes());
        let dynamic_ptr = lib.dynamic_ptr();
        Ok(LibraryInfoRaw {
            name_len,
            compartment_id: handle.comp,
            objid: lib.full_obj.id(),
            slot: lib.full_obj.load_addr() / MAX_SIZE,
            start: (lib.full_obj.load_addr() + NULLPAGE_SIZE) as *mut _,
            len: MAX_SIZE - NULLPAGE_SIZE * 2,
            dl_info: twizzler_rt_abi::debug::DlPhdrInfo {
                addr: lib.base_addr(),
                name: core::ptr::null(),
                phdr: lib.get_phdrs_raw().ok_or(GenericError::Internal)?.0 as *const _,
                phnum: lib.get_phdrs_raw().ok_or(GenericError::Internal)?.1 as u32,
                adds: 0,
                subs: 0,
                tls_modid: lib.tls_id.map(|t| t.tls_id()).unwrap_or(0) as usize,
                tls_data: core::ptr::null_mut(),
            },
            desc,
            link_map: LinkMap(link_map {
                next: core::ptr::null_mut(),
                prev: core::ptr::null_mut(),
                name: core::ptr::null_mut(),
                ld: dynamic_ptr.unwrap_or(core::ptr::null_mut()).cast(),
                addr: lib.base_addr(),
            }),
        })
    }

    /// Open a handle to the n'th library for a compartment.
    pub fn get_library_handle(
        &self,
        caller: ObjID,
        comp: Option<Descriptor>,
        num: usize,
    ) -> Result<Descriptor, TwzError> {
        let (_, ref mut comps, ref dynlink, ref mut handles, ref comphandles) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let comp_id = comp
            .map(|comp| comphandles.lookup(caller, comp).map(|ch| ch.instance))
            .unwrap_or(Some(caller))
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        let rc = comps.get(comp_id)?;
        // TODO: dynlink err map
        let dcomp = dynlink
            .get_compartment(rc.compartment_id)
            .map_err(|_| GenericError::Internal)?;
        let id = dcomp
            .library_ids()
            .nth(num)
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        handles
            .insert(caller, LibraryHandle { comp: comp_id, id })
            .ok_or(ResourceError::OutOfResources.into())
    }

    /// Load a library by name into the caller's compartment.
    /// The name is read from the caller's per-thread simple buffer.
    /// If the library is already loaded, returns a handle to the existing instance.
    pub fn load_library_by_name(
        &self,
        caller: ObjID,
        thread: ObjID,
        name_len: usize,
        id: Option<ObjID>,
    ) -> Result<(Descriptor, usize), TwzError> {
        let (_, ref mut comps, ref mut dynlink, ref mut handles, _) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let rc = comps.get_mut(caller)?;
        let name_bytes = rc.get_per_thread(thread).read_bytes(name_len);
        let name = std::str::from_utf8(&name_bytes)
            .map_err(|_| ArgumentError::InvalidArgument)?
            .to_string();
        let comp_id = rc.compartment_id;

        // If already loaded in this compartment, return a handle to it.
        let (lib_id, loads) = if let Some(id) = dynlink.lookup_library(comp_id, &name) {
            (id, None)
        } else {
            // Load the library and all its dependencies into the caller's compartment.
            let unlib = if let Some(id) = id {
                UnloadedLibrary::new_object(name.clone(), id)
            } else {
                UnloadedLibrary::new(name.clone())
            };
            let mut load_ctx = LoadCtx::default();
            let loads = dynlink
                .load_library_in_compartment(comp_id, unlib, AllowedGates::Private, &mut load_ctx)
                .map_err(|_| TwzError::NOT_FOUND)?;
            tracing::info!("loaded library '{}', got loads: {:#?}", name, loads);
            let root_id = loads.first().ok_or(GenericError::Internal)?.lib;
            // Relocate the newly loaded library graph.
            dynlink
                .relocate_all(root_id)
                .map_err(|_| GenericError::Internal)?;
            (root_id, Some(loads))
        };

        let ctors = if loads.is_none() {
            vec![]
        } else {
            dynlink
                .build_ctors_list(lib_id, Some(comp_id), loads)
                .map_err(|_| TwzError::INVALID_ARGUMENT)?
        };

        let bytes = unsafe {
            core::slice::from_raw_parts(
                ctors.as_ptr().cast::<u8>(),
                ctors.len() * core::mem::size_of::<ctor_set>(),
            )
        };
        let ctor_len = rc.get_per_thread(thread).write_bytes(bytes);

        handles
            .insert(
                caller,
                LibraryHandle {
                    comp: caller,
                    id: lib_id,
                },
            )
            .ok_or(ResourceError::OutOfResources.into())
            .map(|h| (h, ctor_len))
    }

    /// Drop a library handle.
    pub fn drop_library_handle(&self, caller: ObjID, desc: Descriptor) {
        //tracing::info!("drop: {}", desc);
        self.library_handles
            .write(ThreadKey::get().unwrap())
            .remove(caller, desc);
    }

    /// Look up a symbol by name in the given library (or all libs in the caller's compartment
    /// if `lib_desc` is `None`). The symbol name is read from the caller's per-thread simple
    /// buffer. Returns the relocated symbol address.
    pub fn lookup_symbol(
        &self,
        caller: ObjID,
        thread: ObjID,
        lib_desc: Option<Descriptor>,
        name_len: usize,
    ) -> Result<usize, TwzError> {
        let (_, ref mut comps, ref dynlink, ref libhandles, _) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let rc = comps.get_mut(caller)?;
        let name_bytes = rc.get_per_thread(thread).read_bytes(name_len);
        let name = std::str::from_utf8(&name_bytes).map_err(|_| ArgumentError::InvalidArgument)?;

        tracing::info!(
            "looking up symbol '{}' for caller {}, lib_desc = {:?}",
            name,
            caller,
            lib_desc
        );
        match lib_desc {
            Some(desc) => {
                let lib = libhandles
                    .lookup(caller, desc)
                    .ok_or(ArgumentError::InvalidArgument)?;
                let deps = dynlink.build_deps_search_list(lib.id);
                let sym = dynlink
                    .lookup_symbol(
                        lib.id,
                        name,
                        LookupFlags::SKIP_SECGATE_CHECK | LookupFlags::ALLOW_WEAK,
                        &deps,
                    )
                    .map_err(|_| TwzError::NOT_FOUND)?;
                return Ok(sym.reloc_value() as usize);
            }
            None => {
                // RTLD_DEFAULT: start from the compartment's root (first) library.
                for lid in dynlink
                    .get_compartment(rc.compartment_id)
                    .map_err(|_| GenericError::Internal)?
                    .library_ids()
                {
                    let deps = dynlink.build_deps_search_list(lid);
                    if let Ok(sym) = dynlink.lookup_symbol(
                        lid,
                        name,
                        LookupFlags::SKIP_SECGATE_CHECK | LookupFlags::ALLOW_WEAK,
                        &deps,
                    ) {
                        return Ok(sym.reloc_value() as usize);
                    }
                }
            }
        }
        Err(TwzError::NOT_FOUND)
    }
}
