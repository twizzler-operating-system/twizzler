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
    /// Add a note to an object.
    AddNote,
    /// Remove a note from an object.
    RemoveNote(u64),
    /// Get a note from an object.
    GetNote(u64),
    /// Enumerate all notes from an object.
    EnumerateNotes(u64),
}

impl From<ObjectControlCmd> for (u64, u64) {
    fn from(c: ObjectControlCmd) -> Self {
        match c {
            ObjectControlCmd::CreateCommit => (0, 0),
            ObjectControlCmd::Delete(x) => (1, x.bits()),
            ObjectControlCmd::Sync => (2, 0),
            ObjectControlCmd::Preload => (3, 0),
            ObjectControlCmd::AddNote => (4, 0),
            ObjectControlCmd::RemoveNote(x) => (5, x),
            ObjectControlCmd::GetNote(x) => (6, x),
            ObjectControlCmd::EnumerateNotes(x) => (7, x),
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
            4 => ObjectControlCmd::AddNote,
            5 => ObjectControlCmd::RemoveNote(value.1),
            6 => ObjectControlCmd::GetNote(value.1),
            7 => ObjectControlCmd::EnumerateNotes(value.1),
            _ => return Err(ArgumentError::InvalidArgument.into()),
        })
    }
}

/// Perform a kernel operation on this object.
pub fn sys_object_ctrl(id: ObjID, cmd: ObjectControlCmd, arg: u64, arg2: u64) -> Result<u64> {
    let [hi, lo] = id.parts();
    let (cmd, opts) = cmd.into();
    let args = [hi, lo, cmd, opts, arg, arg2];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectCtrl, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v, twzerr)
}

/// Add a note to an object. Returns the note key.
pub fn sys_object_add_note(id: ObjID, value: &[u8]) -> Result<u64> {
    let cmd = ObjectControlCmd::AddNote;
    return sys_object_ctrl(id, cmd, value.as_ptr() as u64, value.len() as u64);
}

pub fn sys_object_remove_note(id: ObjID, key: u64) -> Result<()> {
    let cmd = ObjectControlCmd::RemoveNote(key);
    sys_object_ctrl(id, cmd, 0, 0).map(|_| ())
}

pub fn sys_object_get_note(id: ObjID, key: u64, buf: &mut [u8]) -> Result<usize> {
    let cmd = ObjectControlCmd::GetNote(key);
    let res = sys_object_ctrl(id, cmd, buf.as_mut_ptr() as u64, buf.len() as u64)?;
    Ok(res as usize)
}

pub fn sys_object_enumerate_notes(id: ObjID, offset: usize, buf: &mut [u64]) -> Result<usize> {
    let cmd = ObjectControlCmd::EnumerateNotes(offset as u64);
    let res = sys_object_ctrl(id, cmd, buf.as_mut_ptr() as u64, buf.len() as u64)?;
    Ok(res as usize)
}

#[macro_export]
macro_rules! write_note {
    ($id:expr, $($arg:tt)*) => {{
        if cfg!(debug_assertions) {
            let s = format!($($arg)*);
            twizzler_abi::syscall::sys_object_add_note($id, s.as_bytes()).unwrap_or(0)
        } else {
            0
        }
    }
    };
}
