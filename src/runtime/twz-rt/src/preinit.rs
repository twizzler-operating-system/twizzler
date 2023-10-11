use core::fmt::Write;

use twizzler_abi::syscall::{sys_kernel_console_write, KernelConsoleWriteFlags};

#[derive(Clone, Copy)]
pub struct PreinitLogger {}

impl core::fmt::Write for PreinitLogger {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.write(s.as_bytes());
        Ok(())
    }
}

impl PreinitLogger {
    pub fn write(&self, data: &[u8]) {
        // we did our best
        let _ = sys_kernel_console_write(data, KernelConsoleWriteFlags::empty());
    }
}

static mut PREINIT_OUTPUT: PreinitLogger = PreinitLogger {};

#[doc(hidden)]
pub fn _print_normal(args: ::core::fmt::Arguments) {
    let _ = unsafe { PREINIT_OUTPUT }.write_fmt(args);
}

#[macro_export]
macro_rules! preinit_print {
    ($($arg:tt)*) => {
        $crate::preinit::_print_normal(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! preinit_println {
    () => {
        $crate::preinit_print!("\n")
    };
    ($fmt:expr) => {
        $crate::preinit_print!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::preinit_print!(concat!($fmt, "\n"), $($arg)*)
    };
}

pub fn preinit_abort() -> ! {
    core::intrinsics::abort()
}

pub fn preinit_unwrap<T>(op: Option<T>) -> T {
    match op {
        Some(item) => item,
        None => {
            preinit_println!(
                "failed to unwrap option: {}",
                core::panic::Location::caller()
            );
            preinit_abort();
        }
    }
}

#[allow(dead_code)]
pub fn preinit_unwrap_result<T, E: core::fmt::Display>(op: Result<T, E>) -> T {
    match op {
        Ok(item) => item,
        Err(e) => {
            preinit_println!(
                "failed to unwrap result: {} at {}",
                e,
                core::panic::Location::caller()
            );
            preinit_abort();
        }
    }
}
