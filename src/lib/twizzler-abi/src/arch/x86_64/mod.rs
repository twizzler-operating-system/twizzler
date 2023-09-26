use crate::object::MAX_SIZE;

pub mod syscall;
pub(crate) mod upcall;

#[cfg(feature = "runtime")]
pub(crate) fn new_thread_tls() -> Option<(usize, *mut u8, usize, usize)> {
    // x86_64 uses variant II for TLS
    crate::runtime::tls::tls_variant2()
}

pub const SLOTS: usize = (1 << 47) / MAX_SIZE;
