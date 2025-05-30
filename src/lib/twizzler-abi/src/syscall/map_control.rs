use twizzler_rt_abi::{
    error::{ArgumentError, TwzError},
    Result,
};

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::arch::syscall::raw_syscall;

bitflags::bitflags! {
    /// Flags to control map synchronization.
    #[derive(Debug, Clone, Copy)]
    pub struct SyncFlags : u64 {
        const DISCARD = 1;
    }
}

/// Possible object control commands for [sys_object_ctrl].
#[derive(Clone, Copy, Debug)]
pub enum MapControlCmd {
    /// Sync an entire mapping
    Sync(SyncFlags),
    /// Invalidate a mapping
    Invalidate,
    /// Update a mapping
    Update,
}

impl From<MapControlCmd> for (u64, u64) {
    fn from(c: MapControlCmd) -> Self {
        match c {
            MapControlCmd::Sync(x) => (1, x.bits()),
            MapControlCmd::Update => (2, 0),
            MapControlCmd::Invalidate => (3, 0),
        }
    }
}

impl TryFrom<(u64, u64)> for MapControlCmd {
    type Error = TwzError;
    fn try_from(value: (u64, u64)) -> Result<Self> {
        Ok(match value.0 {
            1 => MapControlCmd::Sync(
                SyncFlags::from_bits(value.1).ok_or(ArgumentError::InvalidArgument)?,
            ),
            2 => MapControlCmd::Update,
            3 => MapControlCmd::Invalidate,
            _ => return Err(ArgumentError::InvalidArgument.into()),
        })
    }
}

/// Perform a kernel operation on this object.
pub fn sys_map_ctrl(start: *const u8, len: usize, cmd: MapControlCmd, opts2: u64) -> Result<()> {
    let (cmd, opts) = cmd.into();
    let args = [start.addr() as u64, len as u64, cmd, opts, opts2];
    let (code, val) = unsafe { raw_syscall(Syscall::MapCtrl, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}
