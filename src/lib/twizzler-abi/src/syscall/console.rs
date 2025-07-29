use core::time::Duration;

use bitflags::bitflags;
use twizzler_rt_abi::Result;

use super::{convert_codes_to_result, twzerr, Syscall, ThreadSyncSleep};
use crate::arch::syscall::raw_syscall;

bitflags! {
    /// Flags to pass to [sys_kernel_console_read].
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    pub struct KernelConsoleReadFlags: u64 {
        /// If the read would block, return instead.
        const NONBLOCKING = 1;
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u64)]
/// Possible sources for a kernel console read syscall.
pub enum KernelConsoleSource {
    /// Read from the console itself.
    Console = 0,
    /// Read from the kernel write buffer.
    Buffer = 1,
    /// Read from the debug console.
    DebugConsole = 2,
}

impl From<KernelConsoleSource> for u64 {
    fn from(x: KernelConsoleSource) -> Self {
        x as u64
    }
}

impl From<u64> for KernelConsoleSource {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::Buffer,
            2 => Self::DebugConsole,
            _ => Self::Console,
        }
    }
}

impl From<KernelConsoleReadFlags> for u64 {
    fn from(x: KernelConsoleReadFlags) -> Self {
        x.bits()
    }
}

/// Read from the specified kernel console input, placing data into `buffer`.
///
/// Returns the number of bytes read on success.
pub fn sys_kernel_console_read(
    source: KernelConsoleSource,
    buffer: &mut [u8],
    flags: KernelConsoleReadFlags,
) -> Result<usize> {
    sys_kernel_console_read_interruptable(source, buffer, flags, None, None)
}

/// Read from the specified kernel console input, placing data into `buffer`.
///
/// Returns the number of bytes read on success.
pub fn sys_kernel_console_read_interruptable(
    source: KernelConsoleSource,
    buffer: &mut [u8],
    flags: KernelConsoleReadFlags,
    timeout: Option<Duration>,
    waiter: Option<ThreadSyncSleep>,
) -> Result<usize> {
    let timeout = timeout
        .as_ref()
        .map_or(core::ptr::null(), |t| t as *const Duration);
    let waiter = waiter
        .as_ref()
        .map_or(core::ptr::null(), |w| w as *const ThreadSyncSleep);
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::KernelConsoleRead,
            &[
                source.into(),
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                flags.into(),
                timeout as u64,
                waiter as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, twzerr)
}

bitflags! {
    /// Flags to pass to [sys_kernel_console_write].
    #[derive(Debug, Copy, Clone, PartialEq, Eq)]
    pub struct KernelConsoleWriteFlags: u64 {
        /// If the buffer is full, discard this write instead of overwriting old data.
        const DISCARD_ON_FULL = 1;
    }
}

/// Write to the kernel console.
///
/// This writes first to the kernel console buffer, for later reading by
/// [sys_kernel_console_read_buffer], and then writes to the underlying kernel console device (if
/// one is present). By default, if the buffer is full, this write will overwrite old data in the
/// (circular) buffer, but this behavior can be controlled by the `flags` argument.
///
/// This function cannot fail.
pub fn sys_kernel_console_write(
    target: KernelConsoleSource,
    buffer: &[u8],
    flags: KernelConsoleWriteFlags,
) {
    let arg0 = buffer.as_ptr() as usize as u64;
    let arg1 = buffer.len() as u64;
    let arg2 = flags.bits();
    let arg3 = target.into();
    unsafe {
        raw_syscall(Syscall::KernelConsoleWrite, &[arg0, arg1, arg2, arg3]);
    }
}
