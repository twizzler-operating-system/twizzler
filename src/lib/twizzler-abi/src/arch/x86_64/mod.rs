#[cfg(feature = "rt")]
pub(crate) mod rt0;
pub mod syscall;
pub(crate) mod upcall;

#[cfg(feature = "rt")]
pub(crate) fn new_thread_tls() -> Option<(usize, *mut u8, usize, usize)> {
    // x86_64 uses variant II for TLS
    crate::rt1::tls_variant2()
}
