use std::alloc::{AllocError, Layout};

use crate::{
    alloc::{arena::ArenaManifest, Allocator},
    object::{BaseType, ImmutableObject, InitializedObject, MutableObject, Object},
    ptr::{InvPtr, InvPtrBuilder, InvSlice},
    tx::{TxCell, TxError, TxHandle, TxResult},
};

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

#[repr(C)]
pub struct VectorHeader<T> {
    inner: TxCell<VectorInner<T>>,
}

impl<T> Default for VectorHeader<T> {
    fn default() -> Self {
        Self {
            inner: TxCell::new(VectorInner::default()),
        }
    }
}

impl<T> BaseType for VectorHeader<T> {}
