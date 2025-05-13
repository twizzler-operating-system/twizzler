//! Functions for handling upcalls from the kernel.

use twizzler_rt_abi::error::RawTwzError;

pub use crate::arch::upcall::UpcallFrame;
use crate::object::ObjID;

/// Information about an exception.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
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
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
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
    /// Construct a new upcall info for memory fault.
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
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u8)]
pub enum ObjectMemoryError {
    /// Tried to access an object's null page
    NullPageAccess,
    /// Tried to access outside of an object
    OutOfBounds(usize),
    /// Failed to satisfy fault due to backing storage failure
    BackingFailed(RawTwzError),
}

/// Information about a non-object-related memory access violation.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct MemoryContextViolationInfo {
    /// The virtual address that caused the exception.
    pub address: u64,
    /// The kind of memory access.
    pub kind: MemoryAccessKind,
}

impl MemoryContextViolationInfo {
    pub fn new(address: u64, kind: MemoryAccessKind) -> Self {
        Self { address, kind }
    }
}

/// Kinds of memory access.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u8)]
pub enum MemoryAccessKind {
    Read,
    Write,
    InstructionFetch,
}

/// Information about a non-object-related memory access violation.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct SecurityViolationInfo {
    /// The virtual address that caused the violation.
    pub address: u64,
    /// The kind of memory access.
    pub access_kind: MemoryAccessKind,
}

/// Possible upcall reasons and info.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub enum UpcallInfo {
    Exception(ExceptionInfo),
    ObjectMemoryFault(ObjectMemoryFaultInfo),
    MemoryContextViolation(MemoryContextViolationInfo),
    SecurityViolation(SecurityViolationInfo),
}

impl UpcallInfo {
    /// The number of upcall info variants
    pub const NR_UPCALLS: usize = 3;
    /// Get the number associated with this variant
    pub fn number(&self) -> usize {
        match self {
            UpcallInfo::Exception(_) => 0,
            UpcallInfo::ObjectMemoryFault(_) => 1,
            UpcallInfo::MemoryContextViolation(_) => 2,
            UpcallInfo::SecurityViolation(_) => 3,
        }
    }
}

/// A collection of data about this upcall, and the [UpcallInfo] for this
/// particular upcall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub struct UpcallData {
    /// Info for this upcall, including reason and elaboration data.
    pub info: UpcallInfo,
    /// Upcall flags
    pub flags: UpcallHandlerFlags,
    /// Source context
    pub source_ctx: ObjID,
    /// The thread ID for this thread.
    pub thread_id: ObjID,
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
    /// Size of the super stack.
    pub super_stack_size: usize,
    /// Value to use for stack pointer, when switching to supervisor context.
    pub super_thread_ptr: usize,
    /// Supervisor context to use, when switching to supervisor context.
    pub super_ctx: ObjID,
    /// Per-upcall options.
    pub options: [UpcallOptions; UpcallInfo::NR_UPCALLS],
}

impl UpcallTarget {
    /// Construct a new upcall target.
    pub fn new(
        self_address: Option<
            unsafe extern "C-unwind" fn(*mut core::ffi::c_void, *const core::ffi::c_void) -> !,
        >,
        super_address: Option<
            unsafe extern "C-unwind" fn(*mut core::ffi::c_void, *const core::ffi::c_void) -> !,
        >,
        super_stack: usize,
        super_stack_size: usize,
        super_thread_ptr: usize,
        super_ctx: ObjID,
        options: [UpcallOptions; UpcallInfo::NR_UPCALLS],
    ) -> Self {
        Self {
            self_address: self_address.map(|addr| addr as usize).unwrap_or_default(),
            super_address: super_address.map(|addr| addr as usize).unwrap_or_default(),
            super_stack,
            super_thread_ptr,
            super_ctx,
            options,
            super_stack_size,
        }
    }
}

/// The exit code the kernel will use when killing a thread that cannot handle
/// an upcall (e.g. the kernel fails to push the upcall stack frame, or the mode is set
/// to [UpcallMode::Abort]).
pub const UPCALL_EXIT_CODE: u64 = 127;

bitflags::bitflags! {
    /// Flags controlling upcall handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UpcallFlags : u8 {
    /// Whether or not to suspend the thread before handling (or aborting from) the upcall.
    const SUSPEND = 1;
}
}

bitflags::bitflags! {
    /// Flags passed to the upcall handler in [UpcallData].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UpcallHandlerFlags : u8 {
    /// Whether or not to suspend the thread before handling (or aborting from) the upcall.
    const SWITCHED_CONTEXT = 1;
}
}

/// Possible modes for upcall handling.
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
