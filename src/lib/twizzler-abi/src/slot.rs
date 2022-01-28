//! Manage slots in the address space. Currently not finished.

use core::sync::atomic::{AtomicUsize, Ordering};

static C: AtomicUsize = AtomicUsize::new(10); //TODO

/// Allocate a slot in the address space where we could map a new object.
pub fn global_allocate() -> Option<usize> {
    Some(C.fetch_add(1, Ordering::SeqCst))
}

/// Return the vaddr range of a slot (start address, end address).
pub fn to_vaddr_range(slot: usize) -> (usize, usize) {
    // TODO
    let start = slot * (1024 * 1024 * 1024) + 0x1000;
    let end = (slot + 1) * (1024 * 1024 * 1024) - 0x1000;
    (start, end)
}
