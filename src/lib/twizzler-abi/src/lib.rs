//! This library provides a common interface for applications that want to talk to the Twizzler
//! kernel, and defines that interface for both applications and the kernel to follow. It's made of
//! several parts:
//!   1. The Runtime -- see [runtime].
//!   2. System Calls -- see [syscall] and [arch::syscall].
//!   3. Other Application-Kernel ABI definitions (e.g. pager queue entries).
//!
//! # Should I use these APIs?
//! All of these interfaces are potentially unstable and should not be used directly by most
//! programs.

#![cfg_attr(not(feature = "std"), no_std)]
#![feature(naked_functions)]
#![feature(core_intrinsics)]
#![feature(int_roundings)]
#![feature(thread_local)]
#![feature(pointer_byte_offsets)]
#![feature(auto_traits)]
#![feature(negative_impls)]
#![allow(internal_features)]
#![feature(rustc_attrs)]
#![feature(asm_const)]
#![feature(linkage)]
#![feature(error_in_core)]
pub mod arch;

#[allow(unused_extern_crates)]
extern crate alloc as rustc_alloc;

pub mod aux;
pub mod device;
pub mod kso;
pub mod marker;
pub mod meta;
pub mod object;
pub mod pager;
#[cfg(feature = "runtime")]
pub mod runtime;
pub mod slot;
pub mod syscall;
pub mod thread;
pub mod upcall;

#[inline]
unsafe fn internal_abort() -> ! {
    cfg_if::cfg_if! {
    if #[cfg(feature = "runtime")] {
        let runtime = twizzler_runtime_api::get_runtime();
        runtime.abort();
    } else {
        core::intrinsics::abort();
    }
    }
}

fn print_err(err: &str) {
    syscall::sys_kernel_console_write(err.as_bytes(), syscall::KernelConsoleWriteFlags::empty());
}

#[allow(dead_code)]
/// during runtime init, we need to call functions that might fail, but if they do so, we should
/// just abort. the standard unwrap() function for option will call panic, but we can't use that, as
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

#[allow(dead_code)]
/// during runtime init, we need to call functions that might fail, but if they do so, we should
/// just abort. the standard unwrap() function for result will call panic, but we can't use that, as
/// the runtime init stuff runs before the panic runtime is ready.
fn internal_unwrap_result<T, E>(t: Result<T, E>, msg: &str) -> T {
    if let Ok(t) = t {
        t
    } else {
        print_err(msg);
        unsafe {
            internal_abort();
        }
    }
}
