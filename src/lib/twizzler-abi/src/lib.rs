//! This library provides a common interface for applications that want to talk to the Twizzler
//! kernel, and defines that interface for both applications and the kernel to follow. It's made of
//! several parts:
//!   1. System Calls -- see [syscall] and [arch::syscall].
//!   2. Other Application-Kernel ABI definitions (e.g. pager queue entries).
//!
//! # Should I use these APIs?
//! All of these interfaces are potentially unstable and should not be used directly by most
//! programs.

#![no_std]
#![feature(naked_functions)]
#![feature(core_intrinsics)]
#![feature(int_roundings)]
#![feature(thread_local)]
#![feature(auto_traits)]
#![feature(negative_impls)]
#![allow(internal_features)]
#![feature(rustc_attrs)]
#![feature(linkage)]
#![feature(test)]
#![feature(c_variadic)]

use syscall::KernelConsoleSource;
pub mod arch;

#[allow(unused_extern_crates)]
extern crate alloc as rustc_alloc;

pub mod aux;
pub mod device;
pub mod klog;
pub mod kso;
pub mod marker;
pub mod meta;
pub mod object;
pub mod pager;
pub mod security;
pub mod simple_mutex;
pub mod slot;
pub mod syscall;
pub mod thread;
pub mod trace;
pub mod upcall;

#[inline]
unsafe fn internal_abort() -> ! {
    core::intrinsics::abort();
}

pub fn print_err(err: &str) {
    syscall::sys_kernel_console_write(
        KernelConsoleSource::Console,
        err.as_bytes(),
        syscall::KernelConsoleWriteFlags::empty(),
    );
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

#[cfg(test)]
extern crate test;

#[cfg(test)]
mod tester {
    #[bench]
    fn test_bench(bench: &mut test::Bencher) {
        bench.iter(|| {
            for i in 0..10000 {
                core::hint::black_box(i);
            }
        });
    }
}
