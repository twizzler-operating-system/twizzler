use twizzler_rt_abi::{
    bindings::sync_info,
    error::{ArgumentError, TwzError},
    Result,
};

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::arch::syscall::raw_syscall;

/// Possible map control commands for [sys_map_ctrl].
#[derive(Clone, Copy, Debug)]
pub enum MapControlCmd {
    /// Sync an entire mapping
    Sync(*const sync_info),
    /// Discard a mapping
    Discard,
    /// Invalidate a mapping
    Invalidate,
    /// Update a mapping
    Update,
}

impl From<MapControlCmd> for (u64, u64) {
    fn from(c: MapControlCmd) -> Self {
        match c {
            MapControlCmd::Sync(x) => (0, x.addr() as u64),
            MapControlCmd::Discard => (1, 0),
            MapControlCmd::Invalidate => (2, 0),
            MapControlCmd::Update => (3, 0),
        }
    }
}

impl TryFrom<(u64, u64)> for MapControlCmd {
    type Error = TwzError;
    fn try_from(value: (u64, u64)) -> Result<Self> {
        Ok(match value.0 {
            0 => MapControlCmd::Sync((value.1 as usize) as *const sync_info),
            1 => MapControlCmd::Discard,
            2 => MapControlCmd::Invalidate,
            3 => MapControlCmd::Update,
            _ => return Err(ArgumentError::InvalidArgument.into()),
        })
    }
}

/// Perform a kernel operation on this mapping.
pub fn sys_map_ctrl(start: *const u8, len: usize, cmd: MapControlCmd, opts2: u64) -> Result<()> {
    let (cmd, opts) = cmd.into();
    let args = [start.addr() as u64, len as u64, cmd, opts, opts2];
    let (code, val) = unsafe { raw_syscall(Syscall::MapCtrl, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}
