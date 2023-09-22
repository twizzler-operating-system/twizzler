use crate::object::MAX_SIZE;

pub mod syscall;
pub(crate) mod upcall;

#[cfg(feature = "runtime")]
pub(crate) fn new_thread_tls() -> Option<(usize, *mut u8, usize, usize)> {
    // x86_64 uses variant II for TLS
    crate::runtime::tls::tls_variant2()
}

/// Return the vaddr range of a slot (start address, end address).
pub fn to_vaddr_range(slot: usize) -> (usize, usize) {
    // TODO
    let start = slot * (1024 * 1024 * 1024) + 0x1000;
    let end = (slot + 1) * (1024 * 1024 * 1024) - 0x1000;
    (start, end)
}

pub const SLOTS: usize = (1 << 47) / MAX_SIZE;
