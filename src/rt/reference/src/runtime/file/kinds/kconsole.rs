use std::time::Duration;

use twizzler_abi::syscall::{
    sys_kernel_console_read, sys_kernel_console_write, KernelConsoleSource, KernelConsoleWriteFlags,
};
use twizzler_rt_abi::{fd::FdFlags, io::IoFlags, Result};

use crate::runtime::file::Fd;

pub struct KernelConsoleFile {}

impl KernelConsoleFile {
    pub fn new() -> Self {
        Self {}
    }
}

impl Fd for KernelConsoleFile {
    fn read(
        &self,
        buf: &mut [u8],
        flags: twizzler_rt_abi::io::IoFlags,
        offset: Option<u64>,
        ep: Option<&mut twizzler_rt_abi::io::Endpoint>,
    ) -> twizzler_rt_abi::Result<usize> {
        sys_kernel_console_read(
            KernelConsoleSource::Console,
            buf,
            if flags.contains(IoFlags::NONBLOCKING) {
                KernelConsoleReadFlags::NONBLOCKING
            } else {
                KernelConsoleReadFlags::empty()
            },
        )
    }

    fn write(
        &self,
        buf: &[u8],
        _flags: twizzler_rt_abi::io::IoFlags,
        _offset: Option<u64>,
        _to: Option<&twizzler_rt_abi::io::Endpoint>,
    ) -> twizzler_rt_abi::Result<usize> {
        sys_kernel_console_write(
            KernelConsoleSource::Console,
            buf,
            KernelConsoleWriteFlags::empty(),
        );
        Ok(buf.len())
    }

    fn stat(&self) -> twizzler_rt_abi::Result<twizzler_rt_abi::fd::FdInfo> {
        Ok(twizzler_rt_abi::fd::FdInfo {
            size: 0,
            flags: FdFlags::IS_TERMINAL,
            kind: twizzler_rt_abi::fd::FdKind::Other,
            id: 0,
            created: Duration::ZERO,
            accessed: Duration::ZERO,
            modified: Duration::ZERO,
            unix_mode: S_IFCHR | 0o666,
        })
    }
}
