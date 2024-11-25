//! Utilities that enable formatted printing for early runtime init.

use std::fmt;

use twizzler_abi::syscall::{sys_kernel_console_write, KernelConsoleWriteFlags};

#[repr(C)]
struct PreinitLogger;

impl fmt::Write for PreinitLogger {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        sys_kernel_console_write(s.as_bytes(), KernelConsoleWriteFlags::empty());
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print_normal(args: core::fmt::Arguments) {
    use fmt::Write;
    let _ = PreinitLogger.write_fmt(args);
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

#[track_caller]
pub fn preinit_abort() -> ! {
    unsafe { core::intrinsics::abort() }
}

#[track_caller]
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

#[track_caller]
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
