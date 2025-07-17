use twizzler_rt_abi::{
    error::{ArgumentError, TwzError},
    Result,
};

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};

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
    /// Sync an entire object (non-transactionally)
    Sync,
    /// Preload an object's data
    Preload,
}

impl From<ObjectControlCmd> for (u64, u64) {
    fn from(c: ObjectControlCmd) -> Self {
        match c {
            ObjectControlCmd::CreateCommit => (0, 0),
            ObjectControlCmd::Delete(x) => (1, x.bits()),
            ObjectControlCmd::Sync => (2, 0),
            ObjectControlCmd::Preload => (3, 0),
        }
    }
}

impl TryFrom<(u64, u64)> for ObjectControlCmd {
    type Error = TwzError;
    fn try_from(value: (u64, u64)) -> Result<Self> {
        Ok(match value.0 {
            0 => ObjectControlCmd::CreateCommit,
            1 => ObjectControlCmd::Delete(
                DeleteFlags::from_bits(value.1).ok_or(ArgumentError::InvalidArgument)?,
            ),
            2 => ObjectControlCmd::Sync,
            3 => ObjectControlCmd::Preload,
            _ => return Err(ArgumentError::InvalidArgument.into()),
        })
    }
}

/// Perform a kernel operation on this object.
pub fn sys_object_ctrl(id: ObjID, cmd: ObjectControlCmd) -> Result<()> {
    let [hi, lo] = id.parts();
    let (cmd, opts) = cmd.into();
    let args = [hi, lo, cmd, opts];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectCtrl, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}
