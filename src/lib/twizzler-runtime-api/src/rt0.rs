#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

use crate::{AuxEntry, LibstdEntry};

unsafe extern "C" fn entry(arg: usize) -> ! {
    rust_entry(arg as *const AuxEntry)
}

unsafe fn rust_entry(arg: *const AuxEntry) -> ! {
    let runtime = crate::get_runtime();
    runtime.runtime_entry(arg, std_entry_from_runtime)
}

extern "C" {
    static std_entry_from_runtime: LibstdEntry;
}

#[no_mangle]
pub unsafe extern "C" fn __tls_get_addr(arg: usize) -> *const u8 {
    let runtime = crate::get_runtime();
    runtime.tls_get_addr((arg as *const crate::TlsIndex).as_ref().unwrap())
}
