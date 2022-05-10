use crate::object::ObjID;

use crate::marker::{BaseTag, BaseVersion};

#[repr(transparent)]
struct MetaFlags(u32);

#[repr(transparent)]
struct Nonce(u128);

#[repr(C)]
pub struct MetaInfo {
    nonce: Nonce,
    kuid: ObjID,
    flags: MetaFlags,
    fotcount: u16,
    extcount: u16,
    tag: BaseTag,
    version: BaseVersion,
}

#[repr(transparent)]
pub struct MetaExtTag(u64);
#[repr(C)]
pub struct MetaExt {
    tag: MetaExtTag,
    value: u64,
}
