use std::mem::MaybeUninit;

use super::{Allocator, OwnedGlobalPtr};
use crate::{
    object::Object,
    ptr::{GlobalPtr, Ref},
    tx::TxRef,
};

pub struct ArenaObject {
    obj: Object<ArenaBase>,
}

impl ArenaObject {
    pub fn new() -> Self {
        todo!()
    }

    pub fn allocator(&self) -> ArenaAllocator {
        todo!()
    }

    pub fn alloc<T>(&self, value: T) -> OwnedGlobalPtr<T, ArenaAllocator> {
        todo!()
    }

    pub fn alloc_inplace<T, F>(
        &self,
        ctor: F,
    ) -> crate::tx::Result<OwnedGlobalPtr<T, ArenaAllocator>>
    where
        F: FnOnce(TxRef<MaybeUninit<T>>) -> crate::tx::Result<TxRef<T>>,
    {
        todo!()
    }
}

#[derive(Clone, Copy)]
pub struct ArenaAllocator {
    ptr: GlobalPtr<ArenaBase>,
}

impl ArenaAllocator {
    pub fn new(ptr: GlobalPtr<ArenaBase>) -> Self {
        Self { ptr }
    }
}

#[repr(C)]
pub struct ArenaBase {}

#[repr(C)]
pub struct ArenaObjBase {}

impl Allocator for ArenaAllocator {
    fn alloc(&self, layout: std::alloc::Layout) -> Result<GlobalPtr<u8>, std::alloc::AllocError> {
        todo!()
    }

    unsafe fn dealloc(&self, ptr: GlobalPtr<u8>, layout: std::alloc::Layout) {
        todo!()
    }
}
