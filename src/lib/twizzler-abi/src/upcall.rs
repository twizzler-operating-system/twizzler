pub use crate::arch::upcall::UpcallFrame;

#[repr(C)]
pub struct ExceptionInfo {
    code: u64,
    info: u64,
}

impl ExceptionInfo {
    pub fn new(code: u64, info: u64) -> Self {
        Self { code, info }
    }
}

#[repr(C)]
pub enum UpcallInfo {
    Exception(ExceptionInfo),
}

#[allow(dead_code)]
pub(crate) fn upcall_rust_entry(_frame: &UpcallFrame, _info: &UpcallInfo) {
    crate::syscall::sys_kernel_console_write(
        b"upcall ent",
        crate::syscall::KernelConsoleWriteFlags::empty(),
    );
    // TODO: check if we have a panic runtime.
    panic!("upcall");
}
