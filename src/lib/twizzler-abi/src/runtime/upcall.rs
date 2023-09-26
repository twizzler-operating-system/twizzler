//! Implements the non-arch-specific upcall handling functionality for the runtime.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::upcall::{UpcallFrame, UpcallInfo};

#[thread_local]
static UPCALL_PANIC: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
pub(crate) fn upcall_rust_entry(frame: &UpcallFrame, info: &UpcallInfo) {
    crate::syscall::sys_kernel_console_write(
        b"upcall ent\n",
        crate::syscall::KernelConsoleWriteFlags::empty(),
    );
    if UPCALL_PANIC.load(Ordering::SeqCst) {
        crate::syscall::sys_thread_exit(127);
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
