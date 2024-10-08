use dynlink::library::LibraryId;
use happylock::ThreadKey;
use secgate::util::Descriptor;
use tracing::Level;
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_runtime_api::{AddrRange, ObjID};

use super::Monitor;
use crate::gates::{LibraryInfo, LoadLibraryError};

/// A handle to a library.
pub struct LibraryHandle {
    comp: ObjID,
    id: LibraryId,
}

impl Monitor {
    /// Get LibraryInfo for a given library handle. Note that this will write to the
    /// compartment-thread's simple buffer.
    #[tracing::instrument(skip(self), level = Level::DEBUG, ret)]
    pub fn get_library_info(
        &self,
        instance: ObjID,
        thread: ObjID,
        desc: Descriptor,
    ) -> Option<LibraryInfo> {
        let (ref mut space, _, ref mut comps, ref dynlink, ref libhandles, _) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let handle = libhandles.lookup(instance, desc)?;
        let lib = dynlink.get_library(handle.id).ok()?;
        let pt = comps.get_mut(instance)?.get_per_thread(thread, space);
        let name_len = pt.write_bytes(lib.name.as_bytes());
        Some(LibraryInfo {
            name_len,
            compartment_id: handle.comp,
            objid: lib.full_obj.object().id,
            slot: lib.base_addr() / MAX_SIZE,
            range: AddrRange {
                start: lib.full_obj.object().start as usize + NULLPAGE_SIZE,
                len: MAX_SIZE - NULLPAGE_SIZE * 2,
            },
            dl_info: twizzler_runtime_api::DlPhdrInfo {
                addr: lib.base_addr(),
                name: core::ptr::null(),
                phdr_start: lib.get_phdrs_raw()?.0 as *const _,
                phdr_num: lib.get_phdrs_raw()?.1 as u32,
                _adds: 0,
                _subs: 0,
                modid: lib.tls_id.map(|t| t.tls_id()).unwrap_or(0) as usize,
                tls_data: core::ptr::null(),
            },
            desc,
        })
    }

    /// Open a handle to the n'th library for a compartment.
    #[tracing::instrument(skip(self), level = Level::DEBUG, ret)]
    pub fn get_library_handle(
        &self,
        caller: ObjID,
        comp: Option<Descriptor>,
        num: usize,
    ) -> Option<Descriptor> {
        let (_, _, ref mut comps, ref dynlink, ref mut handles, ref comphandles) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let comp_id = comp
            .map(|comp| comphandles.lookup(caller, comp).map(|ch| ch.instance))
            .unwrap_or(Some(caller))?;
        let rc = comps.get(comp_id)?;
        let dcomp = dynlink.get_compartment(rc.compartment_id).ok()?;
        let id = dcomp.library_ids().nth(num)?;
        handles.insert(comp_id, LibraryHandle { comp: comp_id, id })
    }

    /// Load a library in the given compartment.
    pub fn load_library(
        &self,
        caller: ObjID,
        id: ObjID,
        comp: Option<Descriptor>,
    ) -> Result<Descriptor, LoadLibraryError> {
        todo!()
    }

    /// Drop a library handle.
    pub fn drop_library_handle(&self, caller: ObjID, desc: Descriptor) {
        self.library_handles
            .write(ThreadKey::get().unwrap())
            .remove(caller, desc);
    }
}
