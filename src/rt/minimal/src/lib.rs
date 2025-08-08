#![cfg_attr(not(feature = "std"), no_std)]
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

use twizzler_abi::syscall::KernelConsoleSource;

pub mod arch;

#[allow(unused_extern_crates)]
extern crate alloc as rustc_alloc;

pub mod runtime;

#[inline]
unsafe fn internal_abort() -> ! {
    runtime::OUR_RUNTIME.abort();
}

pub fn print_err(err: &str) {
    twizzler_abi::syscall::sys_kernel_console_write(
        KernelConsoleSource::Buffer,
        err.as_bytes(),
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
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
    use crate::print_err;

    #[bench]
    fn test_bench(bench: &mut test::Bencher) {
        bench.iter(|| {
            for i in 0..10000 {
                core::hint::black_box(i);
            }
        });
    }
}
