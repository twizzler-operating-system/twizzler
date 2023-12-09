use core::fmt;

use bitflags::bitflags;

use crate::{arch::syscall::raw_syscall, object::ObjID, upcall::UpcallTarget};

use super::{convert_codes_to_result, Syscall};
bitflags! {
    /// Flags to pass to [sys_spawn].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct ThreadSpawnFlags: u32 {
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum UpcallTargetSpawnOption {
    DefaultAbort,
    Inherit,
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
    /// Construct a new ThreadSpawnArgs. If vm_context_handle is Some(handle), then spawn the thread in the
    /// VM context defined by handle. Otherwise spawn it in the same VM context as the spawner.
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

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_spawn].
pub enum ThreadSpawnError {
    /// An unknown error occurred.
    Unknown = 0,
    /// One of the arguments was invalid.   
    InvalidArgument = 1,
    /// A specified object (handle) was not found.
    NotFound = 2,
}

impl ThreadSpawnError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "invalid argument",
            Self::NotFound => "specified object was not found",
        }
    }
}

impl From<ThreadSpawnError> for u64 {
    fn from(x: ThreadSpawnError) -> Self {
        x as u64
    }
}
/*
impl Into<u64> for ThreadSpawnError {
    fn into(self) -> u64 {
        self as u64
    }
}
*/

impl From<u64> for ThreadSpawnError {
    fn from(x: u64) -> Self {
        match x {
            2 => Self::NotFound,
            1 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ThreadSpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ThreadSpawnError {
    fn description(&self) -> &str {
        self.as_str()
    }
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
