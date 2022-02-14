//! Manage slots in the address space. Currently not finished.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    object::Protections,
    syscall::{MapFlags, ObjectCreate, ObjectCreateFlags},
};

struct Context {
    next_slot: AtomicUsize,
}

#[allow(dead_code)]
const RESERVED_TEXT: usize = 0;
#[allow(dead_code)]
const RESERVED_DATA: usize = 1;
#[allow(dead_code)]
const RESERVED_STACK: usize = 2;
const RESERVED_CTX: usize = 7;
const ALLOC_START: usize = 10;

fn get_context_object() -> &'static Context {
    let (start, _) = to_vaddr_range(RESERVED_CTX);
    unsafe { (start as *const Context).as_ref().unwrap() }
}

pub fn runtime_init() {
    let cs = ObjectCreate::new(
        crate::syscall::BackingType::Normal,
        crate::syscall::LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );
    let ctx_object = match crate::syscall::sys_object_create(cs, &[], &[]) {
        Ok(o) => o,
        Err(_) => {
            crate::print_err("failed to allocate initial context object");
            unsafe { crate::internal_abort() }
        }
    };
    if crate::syscall::sys_object_map(
        None,
        ctx_object,
        RESERVED_CTX,
        Protections::READ | Protections::WRITE,
        MapFlags::empty(),
    )
    .is_err()
    {
        crate::print_err("failed to map initial context object");
        unsafe {
            crate::internal_abort();
        }
    }
    let context = get_context_object();
    context.next_slot.store(ALLOC_START, Ordering::SeqCst);
}

/// Allocate a slot in the address space where we could map a new object.
pub fn global_allocate() -> Option<usize> {
    let context = get_context_object();
    Some(context.next_slot.fetch_add(1, Ordering::SeqCst))
}

/// Return the vaddr range of a slot (start address, end address).
pub fn to_vaddr_range(slot: usize) -> (usize, usize) {
    // TODO
    let start = slot * (1024 * 1024 * 1024) + 0x1000;
    let end = (slot + 1) * (1024 * 1024 * 1024) - 0x1000;
    (start, end)
}

/// Release a slot for reuse.
pub fn global_release(_slot: usize) {}
