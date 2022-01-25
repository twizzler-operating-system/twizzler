use core::sync::atomic::{AtomicUsize, Ordering};

static C: AtomicUsize = AtomicUsize::new(10); //TODO

pub fn global_allocate() -> usize {
    C.fetch_add(1, Ordering::SeqCst)
}

pub fn to_vaddr_range(slot: usize) -> (usize, usize) {
    // TODO
    let start = slot * (1024 * 1024 * 1024) + 0x1000;
    let end = (slot + 1) * (1024 * 1024 * 1024) - 0x1000;
    (start, end)
}
