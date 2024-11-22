/// TLS Descriptor Resolver Function
///
/// TLS descriptors are two consecutive GOT entries that define
/// a function and an argument that are used to access variables
/// that live in the static TLS block. Such an access mode is an
/// optmization over __tls_get_addr which needs to lookup a variable
/// by module index and offset. TLS Descriptors are used by default
/// on some architechtures like ARM for example.
///
/// More on TLS Descriptors can be found here:
///     https://www.fsfla.org/~lxoliva/writeups/TLS/RFC-TLSDESC-ARM.txt
pub type TlsDescResolver = unsafe extern "C" fn(*const TlsDesc);

/// A TLS Descriptor.
pub struct TlsDesc {
    /// A pointer to the TLS Descriptor resolver function.
    pub resolver: *const TlsDescResolver,
    /// The argument to be used by the TLS Descriptor resolver function.
    pub value: u64,
}

#[cfg(feature = "rustc-dep-of-std")]
#[cfg(target_arch = "aarch64")]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn _tlsdesc_static(desc: *const TlsDesc) {
    // The offset for the variable in the static TLS block is
    // simply the second word from the TLS descriptor.
    // The result is returned in x0.
    core::arch::asm!("ldr x0, [x0, #8]", "ret", options(noreturn));
}
