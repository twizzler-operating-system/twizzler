use core::sync::atomic::{AtomicU64, Ordering};

use twizzler_rt_abi::{
    error::{ArgumentError, RawTwzError, ResourceError, TwzError},
    Result,
};

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::arch::syscall::raw_syscall;

bitflags::bitflags! {
    /// Flags for a sync command.
    #[derive(Debug, Clone, Copy)]
    pub struct SyncFlags: u32 {
        /// Sync updates to durable storage
        const DURABLE = 1 << 0;
        /// Write release before triggering durable writeback
        const ASYNC_DURABLE = 1 << 0;
    }
}

/// Parameters for the kernel for syncing a mapping.
#[derive(Debug, Clone, Copy)]
pub struct SyncInfo {
    /// Pointer to the wait word for transaction completion.
    pub release: *const AtomicU64,
    /// Value to compare against the wait word.
    pub release_compare: u64,
    /// Value to set if the wait word matches the compare value.
    pub release_set: u64,
    /// Pointer to wait word for durability return value.
    pub durable: *const AtomicU64,
    /// Flags for this sync command.
    pub flags: SyncFlags,
}

unsafe impl Send for SyncInfo {}
unsafe impl Sync for SyncInfo {}

impl SyncInfo {
    pub unsafe fn try_release(&self) -> Result<()> {
        self.release
            .as_ref()
            .unwrap()
            .compare_exchange(
                self.release_compare,
                self.release_set,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .map_err(|_| TwzError::Resource(ResourceError::Refused))
            .map(|_| ())
    }

    pub unsafe fn set_durable(&self, err: impl Into<RawTwzError>) {
        if self.durable.is_null() {
            return;
        }

        self.durable
            .as_ref()
            .unwrap()
            .store(err.into().raw(), Ordering::SeqCst);
    }
}

/// Possible map control commands for [sys_map_ctrl].
#[derive(Clone, Copy, Debug)]
pub enum MapControlCmd {
    /// Sync an entire mapping
    Sync(*const SyncInfo),
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
            0 => MapControlCmd::Sync((value.1 as usize) as *const SyncInfo),
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
