use std::{mem::MaybeUninit, sync::atomic::AtomicU64, time::Duration};

use libc::S_IFCHR;
use twizzler_abi::syscall::{
    sys_get_random, GetRandomFlags, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep,
};
use twizzler_rt_abi::{
    bindings::wait_kind,
    fd::{FdFlags, FdInfo, FdKind},
    io::IoFlags,
    Result,
};

use crate::runtime::file::{Fd, WaitpointResult};

/// `/dev/urandom`: reads draw from the kernel CSPRNG, writes are discarded.
pub struct URandomFile;

// The sleep condition (word == 1) never holds, so a waiter returns immediately.
static URANDOM_WAITWORD: AtomicU64 = AtomicU64::new(0);

impl Fd for URandomFile {
    fn read(
        &self,
        buf: &mut [u8],
        _flags: IoFlags,
        _offset: Option<u64>,
        _ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> Result<usize> {
        let dest = unsafe {
            core::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<MaybeUninit<u8>>(), buf.len())
        };
        sys_get_random(dest, GetRandomFlags::empty())
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
            nlink: 1,
        })
    }

    fn waitpoint(&self, _kind: wait_kind) -> Result<WaitpointResult> {
        Ok(WaitpointResult {
            sleep: ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&URANDOM_WAITWORD),
                1,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ),
            ready: true,
            keepalive: None,
        })
    }
}
