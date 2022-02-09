//! This library provides a common interface for applications that want to talk to the Twizzler
//! kernel, and defines that interface for both applications and the kernel to follow. It's made of
//! several parts:
//!   1. The Runtime -- see [rt1], [aux], and [exec]
//!   2. System Calls -- see [syscall] and [arch::syscall]
//!   3. The rest, acting as support for the rust standard library.
//!
//! # Should I use these APIs?
//! All of these interfaces are potentially unstable and should not be used directly by most
//! programs. Instead, the twizzler crate can be used to access standard Twizzler object APIs, and
//! the rust standard library should be used for general rust programming. Both of these wrap calls
//! to this library in better, more easily consumed APIs.

#![cfg_attr(not(feature = "std"), no_std)]
#![feature(asm)]
#![feature(naked_functions)]
#![feature(core_intrinsics)]
pub mod arch;

pub mod alloc;
pub mod aux;
pub mod device;
#[cfg(any(doc, feature = "rt"))]
pub mod exec;
pub mod kso;
mod llalloc;
pub mod object;
#[cfg(any(doc, feature = "rt"))]
pub mod rt1;
mod simple_idcounter;
pub mod simple_mutex;
pub mod slot;
pub mod syscall;
pub mod thread;
pub mod time;

/// Simple callback into twizzler_abi made by the standard library once it has initialized the
/// environment. No panic runtime is available yet.
pub fn ready() {}

/// We need to provide a no-mangled abort() call for things like libunwind.
#[cfg(feature = "rt")]
#[no_mangle]
pub extern "C" fn abort() -> ! {
    unsafe { internal_abort() }
}

#[inline]
unsafe fn internal_abort() -> ! {
    core::intrinsics::abort();
}

fn print_err(err: &str) {
    syscall::sys_kernel_console_write(err.as_bytes(), syscall::KernelConsoleWriteFlags::empty());
}

/// We need to provide a basic hook for reporting stack check failure in case we link to a library
/// that assumes stack protections works (libunwind).
/// # Safety
/// This function cannot be called safely, as it will abort unconditionally.
#[cfg(feature = "rt")]
#[no_mangle]
pub unsafe extern "C" fn __stack_chk_fail() {
    print_err("stack overflow -- aborting");
    abort();
}

/// During runtime init, we need to call functions that might fail, but if they do so, we should
/// just abort. The standard unwrap() function for Option will call panic, but we can't use that, as
/// the runtime init stuff runs before the panic runtime is ready.
fn internal_unwrap<T>(t: Option<T>, msg: &str) -> T {
    if let Some(t) = t {
        t
    } else {
        print_err(msg);
        unsafe {
            internal_abort();
        }
    }
}
