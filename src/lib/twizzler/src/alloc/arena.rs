use std::mem::MaybeUninit;

use twizzler_abi::object::NULLPAGE_SIZE;

use super::{Allocator, OwnedGlobalPtr, SingleObjectAllocator};
use crate::{
    marker::BaseType,
    object::{CreateError, Object, ObjectBuilder, RawObject},
    ptr::{GlobalPtr, Ref},
    tx::{TxCell, TxObject, TxRef},
};

pub struct ArenaObject {
    obj: Object<ArenaBase>,
}

impl ArenaObject {
    pub fn new() -> crate::tx::Result<Self> {
        let obj = ObjectBuilder::default().build(ArenaBase {
            next: TxCell::new((NULLPAGE_SIZE * 2) as u64),
        })?;
        Ok(Self { obj })
    }

    pub fn tx(self) -> crate::tx::Result<TxObject<ArenaBase>> {
        self.obj.tx()
    }

    pub fn allocator(&self) -> ArenaAllocator {
        ArenaAllocator {
            ptr: GlobalPtr::new(self.obj.id(), NULLPAGE_SIZE as u64),
        }
    }

    pub fn alloc<T>(&self, value: T) -> crate::tx::Result<OwnedGlobalPtr<T, ArenaAllocator>> {
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

impl SingleObjectAllocator for ArenaAllocator {}

#[repr(C)]
pub struct ArenaBase {
    next: TxCell<u64>,
}

impl BaseType for ArenaBase {}

impl Allocator for ArenaAllocator {
    fn alloc(&self, layout: std::alloc::Layout) -> Result<GlobalPtr<u8>, std::alloc::AllocError> {
        todo!()
    }

    unsafe fn dealloc(&self, ptr: GlobalPtr<u8>, layout: std::alloc::Layout) {
        todo!()
    }
}

impl TxObject<ArenaBase> {
    pub fn alloc<T>(&self, val: T) -> crate::tx::Result<OwnedGlobalPtr<T, ArenaAllocator>> {
        todo!()
    }
}
