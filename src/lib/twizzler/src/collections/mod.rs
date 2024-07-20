use std::alloc::{AllocError, Layout};

use crate::{
    alloc::{arena::ArenaManifest, Allocator},
    object::{BaseType, ImmutableObject, InitializedObject, MutableObject, Object},
    ptr::{InvPtr, InvPtrBuilder, InvSlice},
    tx::{TxCell, TxError, TxHandle, TxResult},
};

#[derive(twizzler_derive::Invariant)]
#[repr(C)]
struct VectorInner<T> {
    ptr: InvPtr<T>,
    cap: u64,
    len: u64,
}

impl<T> Default for VectorInner<T> {
    fn default() -> Self {
        Self {
            ptr: InvPtr::null(),
            cap: 0,
            len: 0,
        }
    }
}

#[derive(twizzler_derive::Invariant)]
#[repr(C)]
pub struct VectorHeader<T, Alloc: Allocator> {
    inner: TxCell<VectorInner<T>>,
    alloc: Alloc,
}

impl<T, Alloc: Allocator> VectorHeader<T, Alloc> {
    pub fn new_in(alloc: Alloc) -> Self {
        Self {
            inner: TxCell::new(VectorInner::default()),
            alloc,
        }
    }
}

impl<T, Alloc: Allocator> BaseType for VectorHeader<T, Alloc> {}
