use std::{sync::atomic::AtomicU64, time::Duration};

use libc::S_IFCHR;
use twizzler_abi::syscall::{ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep};
use twizzler_rt_abi::{
    bindings::wait_kind,
    fd::{FdFlags, FdInfo, FdKind},
    io::IoFlags,
    Result,
};

use crate::runtime::file::{Fd, WaitpointResult};

/// `/dev/null`: reads are EOF, writes are discarded, waits are always ready.
pub struct NullFile;

// The sleep condition (word == 1) never holds, so a waiter returns immediately.
static NULL_WAITWORD: AtomicU64 = AtomicU64::new(0);

impl Fd for NullFile {
    fn read(
        &self,
        _buf: &mut [u8],
        _flags: IoFlags,
        _offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        Ok(0)
    }

    fn write(
        &self,
        buf: &[u8],
        _flags: IoFlags,
        _offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        Ok(buf.len())
    }

    fn stat(&self) -> Result<FdInfo> {
        Ok(FdInfo {
            size: 0,
            flags: FdFlags::empty(),
            kind: FdKind::Other,
            id: 0,
            created: Duration::ZERO,
            accessed: Duration::ZERO,
            modified: Duration::ZERO,
            unix_mode: S_IFCHR | 0o666,
        })
    }

    fn waitpoint(&self, _kind: wait_kind) -> Result<WaitpointResult> {
        Ok(WaitpointResult {
            sleep: ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&NULL_WAITWORD),
                1,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ),
            also: None,
            ready: true,
            keepalive: None,
        })
    }
}
