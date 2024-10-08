use bitflags::bitflags;
use num_enum::{FromPrimitive, IntoPrimitive};

use super::{convert_codes_to_result, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID, upcall::UpcallTarget};
bitflags! {
    /// Flags to pass to [sys_spawn].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct ThreadSpawnFlags: u32 {
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum UpcallTargetSpawnOption {
    /// Set all sync event handlers to abort by default. Entry addresses will be zero, and upcalls
    /// will not be issued.
    DefaultAbort,
    /// Inherit the upcall target entry address. All supervisor fields are cleared.
    Inherit,
    /// Set the upcall target directly. The following conditions must be met:
    ///   1. The super_ctx field holds the ID of the current thread's active context (prevents priv
    ///      escalation).
    ///   2. The super_entry_address is at most r-x, and at least --x in the super_ctx.
    ///   3. The super_thread_pointer is exactly rw- in the super_ctx.
    ///   4. The super_stack_pointer is exactly rw- in the super_ctx.
    SetTo(UpcallTarget),
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Arguments to pass to [sys_spawn].
pub struct ThreadSpawnArgs {
    pub entry: usize,
    pub stack_base: usize,
    pub stack_size: usize,
    pub tls: usize,
    pub arg: usize,
    pub flags: ThreadSpawnFlags,
    pub vm_context_handle: Option<ObjID>,
    pub upcall_target: UpcallTargetSpawnOption,
}

impl ThreadSpawnArgs {
    /// Construct a new ThreadSpawnArgs. If vm_context_handle is Some(handle), then spawn the thread
    /// in the VM context defined by handle. Otherwise spawn it in the same VM context as the
    /// spawner.
    #[warn(clippy::too_many_arguments)]
    pub fn new(
        entry: usize,
        stack_base: usize,
        stack_size: usize,
        tls: usize,
        arg: usize,
        flags: ThreadSpawnFlags,
        vm_context_handle: Option<ObjID>,
        upcall_target: UpcallTargetSpawnOption,
    ) -> Self {
        Self {
            entry,
            stack_base,
            stack_size,
            tls,
            arg,
            flags,
            vm_context_handle,
            upcall_target,
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error values for [sys_spawn].
pub enum ThreadSpawnError {
    /// An unknown error occurred.
    #[error("unknown error")]
    #[num_enum(default)]
    Unknown = 0,
    /// One of the arguments was invalid.   
    #[error("invalid argument")]
    InvalidArgument = 1,
    /// A specified object (handle) was not found.
    #[error("object handle not found")]
    NotFound = 2,
}

/// Spawn a new thread, returning the ObjID of the thread's handle or an error.
/// # Safety
/// The caller must ensure that the [ThreadSpawnArgs] has sane values.
pub unsafe fn sys_spawn(args: ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    let (code, val) = raw_syscall(Syscall::Spawn, &[&args as *const ThreadSpawnArgs as u64]);
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        crate::object::ObjID::new_from_parts,
        |_, v| ThreadSpawnError::from(v),
    )
}
