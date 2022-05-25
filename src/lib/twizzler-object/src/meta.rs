use twizzler_abi::{
    meta::MetaInfo,
    object::{ObjID, Protections},
};

use crate::{ptr::LeaError, tx::TxHandle, Object};

#[derive(Debug, Clone, Copy)]
#[repr(C)]
struct FotName {
    name: u64,
    resolver: u64,
}

#[repr(C)]
union FotRef {
    id: ObjID,
    name: FotName,
}

#[repr(C)]
pub struct FotEntry {
    outgoing: FotRef,
    flags: u32,
    info: u32,
    refs: u32,
    resv: u32,
}

impl<T> Object<T> {
    pub unsafe fn meta_unchecked(&self) -> &MetaInfo {
        let end = self.slot.vaddr_meta();
        ((end + twizzler_abi::object::NULLPAGE_SIZE / 2) as *const MetaInfo)
            .as_ref()
            .unwrap_unchecked()
    }
}

impl FotEntry {
    pub fn resolve(&self, _tx: &impl TxHandle) -> Result<(ObjID, Protections), LeaError> {
        Ok((
            unsafe { self.outgoing.id },
            Protections::READ | Protections::WRITE,
        ))
    }
}
