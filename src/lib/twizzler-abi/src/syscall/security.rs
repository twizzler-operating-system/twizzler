use core::fmt;

use crate::{arch::syscall::raw_syscall, object::ObjID};

use super::{convert_codes_to_result, Syscall};

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error returns for [sys_sctx_attach].
pub enum SctxAttachError {
    /// An unknown error occurred.
    Unknown = 0,
    /// One of the arguments was invalid.
    InvalidArgument = 1,
    /// A source or tie object was not found.
    ObjectNotFound = 2,
    /// Permission denied.
    PermissionDenied = 3,
}

impl SctxAttachError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "an argument was invalid",
            Self::ObjectNotFound => "a referenced object was not found",
            Self::PermissionDenied => "a source specification had an unsatisfiable range",
        }
    }
}

impl From<SctxAttachError> for u64 {
    fn from(x: SctxAttachError) -> Self {
        x as Self
    }
}

impl From<u64> for SctxAttachError {
    fn from(x: u64) -> Self {
        match x {
            3 => Self::PermissionDenied,
            2 => Self::ObjectNotFound,
            1 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for SctxAttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Attach to a given security context.
pub fn sys_sctx_attach(id: ObjID) -> Result<(), SctxAttachError> {
    let args = [id.split().0, id.split().1, 0, 0, 0];
    let (code, val) = unsafe { raw_syscall(Syscall::SctxAttach, &args) };
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        |_, _| (),
        |_, v| SctxAttachError::from(v),
    )
}
