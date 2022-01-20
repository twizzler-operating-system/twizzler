use bitflags::bitflags;
use core::fmt;

use crate::arch::syscall::raw_syscall;
#[derive(Copy, Clone, Debug)]
#[repr(C)]
/// All possible Synchronous syscalls into the Twizzler kernel.
pub enum Syscall {
    Null,
    /// Read data from the kernel console, either buffer or input.
    KernelConsoleRead,
    /// Write data to the kernel console.
    KernelConsoleWrite,
    /// Sync a thread with other threads using some number of memory words.
    ThreadSync,
    /// General thread control functions.
    ThreadCtrl,
}

impl Syscall {
    /// Return the number associated with this syscall.
    pub fn num(&self) -> u64 {
        *self as u64
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// Possible errors returned by reading from the kernel console's input.
pub enum KernelConsoleReadError {
    /// Operation would block, but non-blocking was requested.
    WouldBlock,
    /// Failed to read because there was no input mechanism made available to the kernel.
    NoSuchDevice,
    /// The input mechanism had an internal error.
    IOError,
}

impl KernelConsoleReadError {
    fn as_str(&self) -> &str {
        match self {
            Self::WouldBlock => "operation would block",
            Self::NoSuchDevice => "no way to read from kernel console physical device",
            Self::IOError => "an IO error occurred",
        }
    }
}

impl fmt::Display for KernelConsoleReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for KernelConsoleReadError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

bitflags! {
    /// Flags to pass to [sys_kernel_console_read].
    pub struct KernelConsoleReadFlags: u64 {
        /// If the read would block, return instead.
        const NONBLOCKING = 1;
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
    _buffer: &mut [u8],
    _flags: KernelConsoleReadFlags,
) -> Result<usize, KernelConsoleReadError> {
    todo!()
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// Possible errors returned by reading from the kernel console's buffer.
pub enum KernelConsoleReadBufferError {
    /// Operation would block, but non-blocking was requested.
    WouldBlock,
}

impl KernelConsoleReadBufferError {
    fn as_str(&self) -> &str {
        match self {
            Self::WouldBlock => "operation would block",
        }
    }
}

impl fmt::Display for KernelConsoleReadBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for KernelConsoleReadBufferError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

bitflags! {
    /// Flags to pass to [sys_kernel_console_read_buffer].
    pub struct KernelConsoleReadBufferFlags: u64 {
        /// If the operation would block, return instead.
        const NONBLOCKING = 1;
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
    _buffer: &mut [u8],
    _flags: KernelConsoleReadBufferFlags,
) -> Result<usize, KernelConsoleReadBufferError> {
    todo!()
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
