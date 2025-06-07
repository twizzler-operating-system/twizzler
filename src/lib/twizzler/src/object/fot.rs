use std::sync::atomic::AtomicU32;

pub use twizzler_abi::meta::{FotEntry, FotFlags};

use crate::ptr::GlobalPtr;

#[repr(C)]
pub struct ResolveRequest {}

#[repr(C)]
pub struct FotResolve {}

impl<T> From<GlobalPtr<T>> for FotEntry {
    fn from(value: GlobalPtr<T>) -> Self {
        Self {
            values: value.id().parts(),
            resolver: 0,
            flags: AtomicU32::new(0),
        }
    }
}
