use core::fmt;

use crate::{arch::syscall::raw_syscall, object::ObjID};

use super::{convert_codes_to_result, justval, Syscall};

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_object_ctrl].
pub enum ObjectControlError {
    /// An unknown error occurred.
    Unknown = 0,
    /// The ID was invalid.
    InvalidID = 1,
    /// An argument was invalid.
    InvalidArgument = 2,
}

impl ObjectControlError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidID => "invalid ID",
            Self::InvalidArgument => "invalid argument",
        }
    }
}

impl From<ObjectControlError> for u64 {
    fn from(x: ObjectControlError) -> u64 {
        x as u64
    }
}

impl From<u64> for ObjectControlError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidID,
            2 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ObjectControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ObjectControlError {
    fn description(&self) -> &str {
        self.as_str()
    }
}
bitflags::bitflags! {
    /// Flags to control operation of the object delete operation.
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
