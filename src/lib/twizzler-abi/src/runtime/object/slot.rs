use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    object::Protections,
    syscall::{MapFlags, ObjectCreate, ObjectCreateFlags},
};

#[allow(dead_code)]
pub const RESERVED_TEXT: usize = 0;
#[allow(dead_code)]
pub const RESERVED_DATA: usize = 1;
#[allow(dead_code)]
pub const RESERVED_STACK: usize = 2;
#[allow(dead_code)]
pub const RESERVED_KERNEL_INIT: usize = 3;
const ALLOC_START: usize = 10;

/// Allocate a slot in the address space where we could map a new object.
pub fn global_allocate() -> Option<usize> {
    todo!()
}

/// Release a slot for reuse.
pub fn global_release(_slot: usize) {
    todo!()
}
