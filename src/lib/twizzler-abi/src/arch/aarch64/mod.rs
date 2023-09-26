pub mod syscall;
pub(crate) mod upcall;

#[cfg(feature = "runtime")]
pub(crate) fn new_thread_tls() -> Option<(usize, *mut u8, usize, usize)> {
    // aarch64 uses variant I for TLS
    crate::runtime::tls::tls_variant1()
}

pub const SLOTS: usize = (1 << 47) / MAX_SIZE;
