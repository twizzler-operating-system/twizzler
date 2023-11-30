//! Functions for handling upcalls from the kernel.

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
    /// The virtual address at which the error occurred.
    pub addr: usize,
}

impl ObjectMemoryFaultInfo {
    pub fn new(
        object_id: ObjID,
        error: ObjectMemoryError,
        access: MemoryAccessKind,
        addr: usize,
    ) -> Self {
        Self {
            object_id,
            error,
            access,
            addr,
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

#[derive(Debug)]
#[repr(C)]
pub struct UpcallData {
    /// Info for this upcall, including reason and elaboration data.
    pub info: UpcallInfo,
    /// The instruction that caused the upcall (or the syscall that entered the kernel).
    pub ip: usize,
}

/// Information for handling an upcall, per-thread. By default, a thread starts with
/// all these fields initialized to zero, and the mode set to [UpcallMode::Abort].
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UpcallTarget {
    /// Address to jump to when handling via a call.
    pub address: usize,
    /// Address of supervisor stack to use, if non-zero. If the value is zero, or refers to memory
    /// within the object that contains the current thread's user stack, then the frame is pushed to
    /// the stack after the current stack frame.
    ///
    /// This means that for normal code, if this value is non-zero, an upcall will change the stack
    /// to this position, and then start pushing the upcall stack frame. If this value is non-zero,
    /// the upcall frame is pushed to the next available frame spot in the stack after the current stack
    /// pointer.
    ///
    /// If we have an exception in a monitor, the monitor will see the upcall frames pushed to the supervisor
    /// stack like a normal upcall (acts as if super_stack is 0 when already in that stack).
    pub super_stack: usize,
    /// The mode to use when handling this upcall.
    pub mode: UpcallMode,
}

/// The exit code the kernel will use when killing a thread that cannot handle
/// an upcall (e.g. the kernel fails to push the upcall stack frame, or the mode is set
/// to [UpcallMode::Abort]).
pub const UPCALL_EXIT_CODE: u64 = 127;

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum UpcallMode {
    /// Do not call anywhere, just exit the thread.
    Abort,
    /// Invoke the upcall handler at the handler address, if canonical.
    /// If the supervisor stack is a valid address and
    /// points to a different object than the current stack, then the stack pointer
    /// is updated to the value of the supervisor stack (see [UpcallTarget::super_stack]).
    /// Otherwise, the stack pointer is moved to be clear of any red zones on the
    /// current stack (if any).
    Call,
    /// Suspend the thread without modifying any registers. The thread can
    /// be resumed later. Note that some upcall causes may be repeated if
    /// not properly handled.
    Suspend,
    /// Setup the thread to invoke the upcall handler at the handler address (see [UpcallMode::Call]).
    /// Before entering userspace, however, suspend the thread.
    SuspendAndCall,
}

impl UpcallTarget {
    pub fn new(
        target: unsafe extern "C" fn(*const UpcallFrame, *const UpcallInfo) -> !,
        super_stack: usize,
        mode: UpcallMode,
    ) -> Self {
        Self {
            address: target as usize,
            super_stack,
            mode,
        }
    }
}
