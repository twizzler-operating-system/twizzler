use std::os::fd::AsRawFd;

use twizzler_abi::syscall::{ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep};
use twizzler_rt_abi::{
    bindings::{WAIT_READ, WAIT_WRITE},
    io::twz_rt_fd_waitpoint,
};

pub trait TwizzlerWaitable {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep;
    fn wait_item_write(&self) -> twizzler_abi::syscall::ThreadSyncSleep;
}

impl<T: AsRawFd> TwizzlerWaitable for T {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let (pt, val) = twz_rt_fd_waitpoint(self.as_raw_fd(), WAIT_READ).unwrap();
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(pt),
            val,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    fn wait_item_write(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let (pt, val) = twz_rt_fd_waitpoint(self.as_raw_fd(), WAIT_WRITE).unwrap();
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(pt),
            val,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}
