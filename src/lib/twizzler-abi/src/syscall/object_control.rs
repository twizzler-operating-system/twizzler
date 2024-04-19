use num_enum::{FromPrimitive, IntoPrimitive};

use super::{convert_codes_to_result, justval, Syscall};
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
/// Possible error returns for [sys_object_ctrl].
pub enum ObjectControlError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
    /// Invalid object ID.
    #[error("invalid object ID")]
    InvalidID = 2,
}

impl core::error::Error for ObjectControlError {}

bitflags::bitflags! {
    /// Flags to control operation of the object delete operation.
    #[derive(Debug, Clone, Copy)]
    pub struct DeleteFlags : u64 {
        const FORCE = 1;
    }
}

/// Possible object control commands for [sys_object_ctrl].
#[derive(Clone, Copy, Debug)]
pub enum ObjectControlCmd {
    /// Commit an object creation.
    CreateCommit,
    /// Delete an object.
    Delete(DeleteFlags),
}

impl From<ObjectControlCmd> for (u64, u64) {
    fn from(c: ObjectControlCmd) -> Self {
        match c {
            ObjectControlCmd::CreateCommit => (0, 0),
            ObjectControlCmd::Delete(x) => (1, x.bits()),
        }
    }
}

impl TryFrom<(u64, u64)> for ObjectControlCmd {
    type Error = ();
    fn try_from(value: (u64, u64)) -> Result<Self, Self::Error> {
        Ok(match value.0 {
            0 => ObjectControlCmd::CreateCommit,
            1 => ObjectControlCmd::Delete(DeleteFlags::from_bits(value.1).ok_or(())?),
            _ => return Err(()),
        })
    }
}

/// Perform a kernel operation on this object.
pub fn sys_object_ctrl(id: ObjID, cmd: ObjectControlCmd) -> Result<(), ObjectControlError> {
    let (hi, lo) = id.split();
    let (cmd, opts) = cmd.into();
    let args = [hi, lo, cmd, opts];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectCtrl, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), justval)
}
