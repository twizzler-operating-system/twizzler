use dynlink::library::LibraryId;
use happylock::ThreadKey;
use secgate::util::Descriptor;
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::{
    error::{ArgumentError, GenericError, ResourceError, TwzError},
    object::ObjID,
};

use super::Monitor;
use crate::gates::LibraryInfo;

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
    ) -> Result<LibraryInfo, TwzError> {
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
        Ok(LibraryInfo {
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

    /// Load a library in the given compartment.
    pub fn load_library(
        &self,
        _caller: ObjID,
        _id: ObjID,
        _comp: Option<Descriptor>,
    ) -> Result<Descriptor, TwzError> {
        todo!()
    }

    /// Drop a library handle.
    pub fn drop_library_handle(&self, caller: ObjID, desc: Descriptor) {
        //tracing::info!("drop: {}", desc);
        self.library_handles
            .write(ThreadKey::get().unwrap())
            .remove(caller, desc);
    }
}
