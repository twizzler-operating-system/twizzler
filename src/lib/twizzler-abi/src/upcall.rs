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

impl UpcallInfo {
    pub fn number(&self) -> usize {
        match self {
            UpcallInfo::Exception(_) => 0,
            UpcallInfo::ObjectMemoryFault(_) => 1,
            UpcallInfo::MemoryContextViolation(_) => 2,
        }
    }
}

// TODO: tie this to the above
pub const NR_UPCALLS: usize = 3;

#[derive(Debug)]
#[repr(C)]
pub struct UpcallData {
    /// Info for this upcall, including reason and elaboration data.
    pub info: UpcallInfo,
    /// Upcall flags
    pub flags: UpcallHandlerFlags,
}

/// Information for handling an upcall, per-thread. By default, a thread starts with
/// all these fields initialized to zero, and the mode set to [UpcallMode::Abort].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub struct UpcallTarget {
    /// Address to jump to when handling via a call to the same context.
    pub self_address: usize,
    /// Address to jump to when handling via a call to supervisor context.
    pub super_address: usize,
    /// Address of supervisor stack to use, when switching to supervisor context.
    pub super_stack: usize,
    /// Value to use for stack pointer, when switching to supervisor context.
    pub super_thread_ptr: usize,
    /// Supervisor context to use, when switching to supervisor context.
    pub super_ctx: ObjID,
    /// Per-upcall options.
    pub options: [UpcallOptions; NR_UPCALLS],
}

impl UpcallTarget {
    pub fn new(
        self_address: unsafe extern "C-unwind" fn(*const UpcallFrame, *const UpcallInfo) -> !,
        super_address: unsafe extern "C-unwind" fn(*const UpcallFrame, *const UpcallInfo) -> !,
        super_stack: usize,
        super_thread_ptr: usize,
        super_ctx: ObjID,
        options: [UpcallOptions; NR_UPCALLS],
    ) -> Self {
        Self {
            self_address: self_address as usize,
            super_address: super_address as usize,
            super_stack,
            super_thread_ptr,
            super_ctx,
            options,
        }
    }
}

/// The exit code the kernel will use when killing a thread that cannot handle
/// an upcall (e.g. the kernel fails to push the upcall stack frame, or the mode is set
/// to [UpcallMode::Abort]).
pub const UPCALL_EXIT_CODE: u64 = 127;

bitflags::bitflags! {
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UpcallFlags : u8 {
    /// Whether or not to suspend the thread before handling (or aborting from) the upcall.
    const SUSPEND = 1;
}
}

bitflags::bitflags! {
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UpcallHandlerFlags : u8 {
    /// Whether or not to suspend the thread before handling (or aborting from) the upcall.
    const SWITCHED_CONTEXT = 1;
}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UpcallMode {
    /// Handle this upcall by immediate abort. If the SUSPEND flag is set, the thread will
    /// still abort when unsuspended.
    Abort,
    /// Handle this upcall by calling, without trying to transfer to supervisor context. Upcall
    /// data, including frame data, will be placed on the current stack, and the thread pointer
    /// is unchanged.
    CallSelf,
    /// Handle this upcall by calling into supervisor context. If the thread is already in
    /// supervisor context, this acts like [UpcallMode::CallSelf]. Otherwise, the thread's stack
    /// and thread pointer are updated to the super_stack and super_thread_pointer values in the
    /// upcall target respectively, and the active security context is switched to the supervisor
    /// context (super_ctx).
    CallSuper,
}

/// Options for a single upcall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UpcallOptions {
    /// Flags for the upcall.
    pub flags: UpcallFlags,
    /// The mode for the upcall.
    pub mode: UpcallMode,
}
