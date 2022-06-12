//! Functions for handling upcalls from the kernel.

use core::sync::atomic::{AtomicBool, Ordering};

pub use crate::arch::upcall::UpcallFrame;

/// Information about an exception.
#[derive(Debug)]
#[repr(C)]
pub struct ExceptionInfo {
    /// CPU-reported exception code.
    pub code: u64,
    /// Arch-specific additional info.
    pub info: u64,
}

impl ExceptionInfo {
    /// Construct new exception info.
    pub fn new(code: u64, info: u64) -> Self {
        Self { code, info }
    }
}

/// Possible upcall reasons and info.
#[derive(Debug)]
#[repr(C)]
pub enum UpcallInfo {
    Exception(ExceptionInfo),
}

#[thread_local]
static UPCALL_PANIC: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
pub(crate) fn upcall_rust_entry(frame: &UpcallFrame, info: &UpcallInfo) {
    crate::syscall::sys_kernel_console_write(
        b"upcall ent\n",
        crate::syscall::KernelConsoleWriteFlags::empty(),
    );
    if UPCALL_PANIC.load(Ordering::SeqCst) {
        crate::syscall::sys_thread_exit(127, core::ptr::null_mut());
    }
    UPCALL_PANIC.store(true, Ordering::SeqCst);
    // TODO: check if we have a panic runtime.
    panic!(
        "upcall ip={:x} sp={:x} :: {:?}",
        frame.ip(),
        frame.sp(),
        info
    );
}
