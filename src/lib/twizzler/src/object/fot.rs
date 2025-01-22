use std::sync::atomic::AtomicU32;

use thiserror::Error;

use crate::ptr::GlobalPtr;

bitflags::bitflags! {
    #[repr(C)]
    #[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Ord, Eq, Hash)]
    pub struct FotFlags : u32 {
        const RESERVED = 1;
        const ACTIVE = 2;
        const RESOLVER = 4;
    }
}

#[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Ord, Eq, Hash, Error)]
#[repr(C)]
pub enum FotError {
    #[error("invalid FOT index")]
    InvalidIndex,
    #[error("invalid FOT entry")]
    InvalidFotEntry,
}

#[repr(C)]
pub struct ResolveRequest {}

#[repr(C)]
pub struct FotResolve {}

#[repr(C)]
pub struct FotEntry {
    pub values: [u64; 2],
    pub resolver: u64,
    pub flags: AtomicU32,
}

impl<T> From<GlobalPtr<T>> for FotEntry {
    fn from(value: GlobalPtr<T>) -> Self {
        Self {
            values: value.id().parts(),
            resolver: 0,
            flags: AtomicU32::new(0),
        }
    }
}
