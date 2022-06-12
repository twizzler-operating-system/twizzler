//! Types that make up object metadata.

use crate::object::ObjID;

use crate::marker::{BaseTag, BaseVersion};

/// Flags for objects.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct MetaFlags(u32);

/// A nonce for avoiding object ID collision.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Nonce(u128);

/// The core metadata that all objects share.
#[repr(C)]
pub struct MetaInfo {
    /// The ID nonce.
    pub nonce: Nonce,
    /// The object's public key ID.
    pub kuid: ObjID,
    /// The object flags.
    pub flags: MetaFlags,
    /// The number of FOT entries.
    pub fotcount: u16,
    /// The number of meta extensions.
    pub extcount: u16,
    /// The tag of the base struct type.
    pub tag: BaseTag,
    /// The version of the base struct type.
    pub version: BaseVersion,
}

/// A tag for a meta extension entry.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct MetaExtTag(u64);

/// A meta extension entry.
#[repr(C)]
pub struct MetaExt {
    /// The tag.
    pub tag: MetaExtTag,
    /// A tag-specific value.
    pub value: u64,
}
