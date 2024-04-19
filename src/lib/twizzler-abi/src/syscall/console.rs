use bitflags::bitflags;
use num_enum::{FromPrimitive, IntoPrimitive};

use crate::arch::syscall::raw_syscall;

use super::{convert_codes_to_result, Syscall};

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible errors returned by reading from the kernel console's input.
pub enum KernelConsoleReadError {
    /// Unknown error.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// Operation would block, but non-blocking was requested.
    #[error("would block")]
    WouldBlock = 1,
    /// Failed to read because there was no input mechanism made available to the kernel.
    #[error("no such device")]
    NoSuchDevice = 2,
    /// The input mechanism had an internal error.
    #[error("I/O error")]
    IOError = 3,
}

impl core::error::Error for KernelConsoleReadError {}

bitflags! {
    /// Flags to pass to [sys_kernel_console_read].
    pub struct KernelConsoleReadFlags: u64 {
        /// If the read would block, return instead.
        const NONBLOCKING = 1;
    }
}

#[repr(u64)]
/// Possible sources for a kernel console read syscall.
pub enum KernelConsoleReadSource {
    /// Read from the console itself.
    Console = 0,
    /// Read from the kernel write buffer.
    Buffer = 1,
}

impl From<KernelConsoleReadSource> for u64 {
    fn from(x: KernelConsoleReadSource) -> Self {
        x as u64
    }
}

impl From<u64> for KernelConsoleReadSource {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::Buffer,
            _ => Self::Console,
        }
    }
}

impl From<KernelConsoleReadFlags> for u64 {
    fn from(x: KernelConsoleReadFlags) -> Self {
        x.bits()
    }
}

/// Read from the kernel console input, placing data into `buffer`.
///
/// This is the INPUT mechanism, and not the BUFFER mechanism. For example, if the kernel console is
/// a serial port, the input mechanism is the reading side of the serial console. To read from the
/// kernel console output buffer, use [sys_kernel_console_read_buffer].
///
/// Returns the number of bytes read on success and [KernelConsoleReadError] on failure.
pub fn sys_kernel_console_read(
    buffer: &mut [u8],
    flags: KernelConsoleReadFlags,
) -> Result<usize, KernelConsoleReadError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::KernelConsoleRead,
            &[
                KernelConsoleReadSource::Console.into(),
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                flags.into(),
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, |_, v| v.into())
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible errors returned by reading from the kernel console's input.
pub enum KernelConsoleReadBufferError {
    /// Unknown error.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// Operation would block, but non-blocking was requested.
    #[error("would block")]
    WouldBlock = 1,
}

impl core::error::Error for KernelConsoleReadBufferError {}

bitflags! {
    /// Flags to pass to [sys_kernel_console_read_buffer].
    pub struct KernelConsoleReadBufferFlags: u64 {
        /// If the operation would block, return instead.
        const NONBLOCKING = 1;
    }
}

impl From<KernelConsoleReadBufferFlags> for u64 {
    fn from(x: KernelConsoleReadBufferFlags) -> Self {
        x.bits()
    }
}

/// Read from the kernel console buffer, placing data into `buffer`.
///
/// This is the BUFFER mechanism, and not the INPUT mechanism. All writes to the kernel console get
/// placed in the buffer and copied out to the underlying console device in the kernel. If you want
/// to read from the INPUT device, see [sys_kernel_console_read].
///
/// Returns the number of bytes read on success and [KernelConsoleReadBufferError] on failure.
pub fn sys_kernel_console_read_buffer(
    buffer: &mut [u8],
    flags: KernelConsoleReadBufferFlags,
) -> Result<usize, KernelConsoleReadBufferError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::KernelConsoleRead,
            &[
                KernelConsoleReadSource::Buffer.into(),
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
                flags.into(),
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, |_, v| v.into())
}

bitflags! {
    /// Flags to pass to [sys_kernel_console_write].
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
pub fn sys_kernel_console_write(buffer: &[u8], flags: KernelConsoleWriteFlags) {
    let arg0 = buffer.as_ptr() as usize as u64;
    let arg1 = buffer.len() as u64;
    let arg2 = flags.bits();
    unsafe {
        raw_syscall(Syscall::KernelConsoleWrite, &[arg0, arg1, arg2]);
    }
}
