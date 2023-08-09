//! Functions for handling upcalls from the kernel.

use core::sync::atomic::{AtomicBool, Ordering};

pub use crate::arch::upcall::UpcallFrame;
use crate::object::ObjID;

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

/// Information about a memory access error to an object.
#[derive(Debug)]
#[repr(C)]
pub struct ObjectMemoryFaultInfo {
    /// Object ID of attempted access.
    pub object_id: ObjID,
    /// The kind of error.
    pub error: ObjectMemoryError,
    /// The kind of memory access that caused the error.
    pub access: MemoryAccessKind,
}

impl ObjectMemoryFaultInfo {
    pub fn new(object_id: ObjID, error: ObjectMemoryError, access: MemoryAccessKind) -> Self {
        Self {
            object_id,
            error,
            access,
        }
    }
}

/// Kinds of object memory errors.
#[derive(Debug)]
#[repr(u8)]
pub enum ObjectMemoryError {
    NullPageAccess,
    OutOfBounds(usize),
}

/// Information about a non-object-related memory access violation.
#[derive(Debug)]
#[repr(C)]
pub struct MemoryContextViolationInfo {
    pub address: u64,
    pub kind: MemoryAccessKind,
}

impl MemoryContextViolationInfo {
    pub fn new(address: u64, kind: MemoryAccessKind) -> Self {
        Self { address, kind }
    }
}

/// Kinds of memory access.
#[derive(Debug, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u8)]
pub enum MemoryAccessKind {
    Read,
    Write,
    InstructionFetch,
}

/// Possible upcall reasons and info.
#[derive(Debug)]
#[repr(C)]
pub enum UpcallInfo {
    Exception(ExceptionInfo),
    ObjectMemoryFault(ObjectMemoryFaultInfo),
    MemoryContextViolation(MemoryContextViolationInfo),
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
