use super::Allocator;
use crate::{object::Object, ptr::GlobalPtr};

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
}

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

impl Allocator for ArenaAllocator {}
