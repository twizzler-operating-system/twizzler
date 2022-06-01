use std::{mem::size_of, ptr::NonNull};

use twizzler_abi::{
    marker::{BaseTag, BaseVersion},
    meta::{MetaExt, MetaFlags, MetaInfo, Nonce},
    object::ObjID,
};

use crate::Object;

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

/// An entry in the FOT.
#[repr(C)]
pub struct FotEntry {
    outgoing: FotRef,
    flags: u32,
    info: u32,
    refs: u32,
    resv: u32,
}

impl<T> Object<T> {
    /// Get a mutable reference to the object's meta info struct.
    ///
    /// # Safety
    /// See this crate's base documentation ([Isolation Safety](crate)).
    pub unsafe fn meta(&self) -> NonNull<MetaInfo> {
        let end = self.slot.vaddr_meta();
        ((end + twizzler_abi::object::NULLPAGE_SIZE / 2) as *mut MetaInfo)
            .as_mut()
            .unwrap_unchecked()
            .into()
    }

    /// Get a mutable reference to the object's first meta extension entry.
    ///
    /// # Safety
    /// See this crate's base documentation ([Isolation Safety](crate)).
    pub unsafe fn metaext(&self) -> NonNull<MetaExt> {
        let end = self.slot.vaddr_meta();
        ((end + twizzler_abi::object::NULLPAGE_SIZE / 2 + size_of::<MetaInfo>()) as *mut MetaExt)
            .as_mut()
            .unwrap_unchecked()
            .into()
    }

    pub fn meta_nonce(&self) -> Nonce {
        unsafe { self.meta().as_mut().nonce }
    }

    pub fn meta_kuid(&self) -> ObjID {
        unsafe { self.meta().as_mut().kuid }
    }

    pub fn meta_flags(&self) -> MetaFlags {
        unsafe { self.meta().as_mut().flags }
    }

    pub fn meta_tag(&self) -> BaseTag {
        unsafe { self.meta().as_mut().tag }
    }

    pub fn meta_version(&self) -> BaseVersion {
        unsafe { self.meta().as_mut().version }
    }
}
