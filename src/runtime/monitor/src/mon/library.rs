use happylock::ThreadKey;
use secgate::util::Descriptor;
use twizzler_abi::object::MAX_SIZE;
use twizzler_runtime_api::{AddrRange, ObjID};

use super::Monitor;
use crate::gates::{LibraryInfo, LoadLibraryError};

impl Monitor {
    pub fn get_library_info(&self, caller: ObjID, desc: Descriptor) -> Option<LibraryInfo> {
        let locks = self.locks.lock(ThreadKey::get().unwrap());
        let handle = locks.4.lookup(caller, desc)?;
        let lib = locks.3.get_library(todo!()).ok()?;
        Some(LibraryInfo {
            id: todo!(),
            name_len: todo!(),
            compartment_id: handle.comp,
            objid: lib.full_obj.object().id,
            slot: lib.base_addr() / MAX_SIZE,
            range: AddrRange {
                start: lib.base_addr(),
                len: MAX_SIZE * 2,
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

    pub fn get_library_handle(
        &self,
        caller: ObjID,
        comp: Option<Descriptor>,
        num: usize,
    ) -> Option<Descriptor> {
        todo!()
    }

    pub fn load_library(&self, caller: ObjID, id: ObjID) -> Result<Descriptor, LoadLibraryError> {
        todo!()
    }

    pub fn drop_library_handle(&self, caller: ObjID, desc: Descriptor) {
        todo!()
    }
}
