use super::Allocator;
use crate::object::Object;

pub struct ArenaAllocator {
    obj: Object<ArenaBase>,
}

#[repr(C)]
pub struct ArenaBase {}

#[repr(C)]
pub struct ArenaObjBase {}

impl Allocator for ArenaAllocator {}
