use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    object::Protections,
    syscall::{MapFlags, ObjectCreate, ObjectCreateFlags},
};

/// Allocate a slot in the address space where we could map a new object.
pub fn global_allocate() -> Option<usize> {
    todo!()
}

/// Release a slot for reuse.
pub fn global_release(_slot: usize) {
    todo!()
}
