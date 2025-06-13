//! Types that make up object metadata.

use core::sync::atomic::AtomicU32;

pub use twizzler_rt_abi::object::{
    MetaExt, MetaExtTag, MetaFlags, MetaInfo, MEXT_EMPTY, MEXT_SIZED,
};

#[repr(C)]
pub struct FotEntry {
    pub values: [u64; 2],
    pub resolver: u64,
    pub flags: AtomicU32,
}

bitflags::bitflags! {
    #[repr(C)]
    #[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
    pub struct FotFlags : u32 {
        const ALLOCATED = 1;
        const ACTIVE = 2;
        const DELETED = 4;
        const RESOLVER = 8;
    }
}
