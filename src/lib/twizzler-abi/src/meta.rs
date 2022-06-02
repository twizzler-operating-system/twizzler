use crate::object::ObjID;

use crate::marker::{BaseTag, BaseVersion};

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct MetaFlags(u32);

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Nonce(u128);

#[repr(C)]
pub struct MetaInfo {
    pub nonce: Nonce,
    pub kuid: ObjID,
    pub flags: MetaFlags,
    pub fotcount: u16,
    pub extcount: u16,
    pub tag: BaseTag,
    pub version: BaseVersion,
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct MetaExtTag(u64);
#[repr(C)]
pub struct MetaExt {
    tag: MetaExtTag,
    value: u64,
}
