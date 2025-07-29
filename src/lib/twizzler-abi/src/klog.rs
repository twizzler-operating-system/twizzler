//! Utilities that enable formatted printing for early runtime init.

use core::fmt;

use crate::syscall::{sys_kernel_console_write, KernelConsoleSource, KernelConsoleWriteFlags};

#[repr(C)]
struct KernelLogger;

impl fmt::Write for KernelLogger {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        sys_kernel_console_write(
            KernelConsoleSource::Console,
            s.as_bytes(),
            KernelConsoleWriteFlags::empty(),
        );
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print_normal(args: core::fmt::Arguments) {
    use fmt::Write;
    let _ = KernelLogger.write_fmt(args);
}

#[macro_export]
macro_rules! klog_print {
    ($($arg:tt)*) => {
        $crate::klog::_print_normal(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! klog_println {
    () => {
        $crate::klog_print!("\n")
    };
    ($fmt:expr) => {
        $crate::klog_print!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::klog_print!(concat!($fmt, "\n"), $($arg)*)
    };
}
