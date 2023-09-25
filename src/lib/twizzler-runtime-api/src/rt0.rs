//! rt0 defines a collection of functions that the basic Rust ABI expects to be defined by some part of the C runtime:
//! 
//!   - __tls_get_addr for handling non-local TLS regions.
//!   - _start, the entry point of an executable (per-arch, as this is assembly code).

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

use crate::AuxEntry;

// The C-based entry point coming from arch-specific assembly _start function.
unsafe extern "C" fn entry(arg: usize) -> ! {
    // Just trampoline to rust-abi code.
    rust_entry(arg as *const AuxEntry)
}

unsafe fn rust_entry(arg: *const AuxEntry) -> ! {
    // All we need to do is grab the runtime and call its init function. We want to
    // do as little as possible here.
    let runtime = crate::get_runtime();
    runtime.runtime_entry(arg, std_entry_from_runtime)
}

extern "C" {
    fn std_entry_from_runtime(aux: super::BasicAux) -> super::BasicReturn;
}

/// Public definition of __tls_get_addr, a function that gets automatically called by the compiler when needed for TLS
/// pointer resolution.
#[no_mangle]
pub unsafe extern "C" fn __tls_get_addr(arg: usize) -> *const u8 {
    // Just call the runtime.
    let runtime = crate::get_runtime();
    runtime.tls_get_addr((arg as *const crate::TlsIndex).as_ref().unwrap())
}
