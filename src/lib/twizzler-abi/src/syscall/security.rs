use num_enum::{FromPrimitive, IntoPrimitive};

use super::{convert_codes_to_result, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    Hash,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error returns for [sys_sctx_attach].
pub enum SctxAttachError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
    /// An was not found.
    #[error("object not found")]
    ObjectNotFound = 2,
    /// Permission denied.
    #[error("permission denied")]
    PermissionDenied = 3,
}

impl core::error::Error for SctxAttachError {}

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
