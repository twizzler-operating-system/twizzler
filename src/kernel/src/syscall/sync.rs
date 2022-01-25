use core::time::Duration;

use twizzler_abi::syscall::{ThreadSync, ThreadSyncError};

pub fn sys_thread_sync(
    _ops: &[ThreadSync],
    _timeout: Option<&mut Duration>,
) -> Result<u64, ThreadSyncError> {
    Ok(0)
}
