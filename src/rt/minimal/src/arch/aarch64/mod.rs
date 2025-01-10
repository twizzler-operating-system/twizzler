use twizzler_abi::object::MAX_SIZE;

pub(crate) mod upcall;

pub(crate) fn new_thread_tls() -> Option<(usize, *mut u8, usize, usize)> {
    // aarch64 uses variant I for TLS
    crate::runtime::tls::tls_variant1()
}

// Max size of user addr space divided into slots of size MAX_SIZE
pub const SLOTS: usize = (1 << 47) / MAX_SIZE;
