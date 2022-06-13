use core::fmt;

use bitflags::bitflags;

use crate::{arch::syscall::raw_syscall, object::ObjID};

use super::{convert_codes_to_result, justval, Syscall};
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_new_handle].
pub enum NewHandleError {
    /// An unknown error occurred.
    Unknown = 0,
    /// One of the arguments was invalid.   
    InvalidArgument = 1,
    /// The specified object is already a handle.
    AlreadyHandle = 2,
    /// The specified object was not found.
    NotFound = 3,
}

impl NewHandleError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "invalid argument",
            Self::AlreadyHandle => "object is already a handle",
            Self::NotFound => "object was not found",
        }
    }
}

impl From<NewHandleError> for u64 {
    fn from(x: NewHandleError) -> Self {
        x as u64
    }
}

impl From<u64> for NewHandleError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for NewHandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for NewHandleError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

/// Possible kernel handle types.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u64)]
pub enum HandleType {
    VmContext,
    KernelToPagerQueue,
    PagerToKernelQueue,
}

impl TryFrom<u64> for HandleType {
    type Error = NewHandleError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::VmContext),
            1 => Ok(Self::KernelToPagerQueue),
            2 => Ok(Self::PagerToKernelQueue),
            _ => Err(NewHandleError::InvalidArgument),
        }
    }
}

bitflags! {
    /// Flags to pass to [sys_new_handle].
    pub struct NewHandleFlags: u64 {
    }
}

/// Make a new handle object.
pub fn sys_new_handle(
    objid: ObjID,
    handle_type: HandleType,
    flags: NewHandleFlags,
) -> Result<u64, NewHandleError> {
    let (hi, lo) = objid.split();
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::NewHandle,
            &[hi, lo, handle_type as u64, flags.bits()],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as u64, justval)
}
