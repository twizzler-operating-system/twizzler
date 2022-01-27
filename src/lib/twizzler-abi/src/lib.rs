#![cfg_attr(not(feature = "std"), no_std)]
#![feature(asm)]
#![feature(naked_functions)]
#![feature(core_intrinsics)]

mod arch;

pub mod alloc;
pub mod aux;
#[cfg(feature = "rt")]
pub mod exec;
mod llalloc;
pub mod object;
#[cfg(feature = "rt")]
mod rt1;
pub mod simple_mutex;
pub mod slot;
pub mod syscall;
pub mod time;

pub fn ready() {}

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

#[no_mangle]
pub unsafe extern "C" fn __stack_chk_fail() {
    print_err("stack overflow -- aborting");
    abort();
}

fn internal_unwrap<T>(t: Option<T>, msg: &str) -> T {
    if let Some(t) = t {
        t
    } else {
        print_err(msg);
        abort();
    }
}
