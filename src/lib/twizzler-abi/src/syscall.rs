use bitflags::bitflags;
use core::fmt;
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub enum Syscall {
    Null,
    KernelConsoleRead,
    KernelConsoleWrite,
    ThreadSync,
    ThreadCtrl,
}

impl Syscall {
    pub fn num(&self) -> u64 {
        *self as u64
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum KernelConsoleReadError {
    WouldBlock,
    NoSuchDevice,
    IOError,
}

impl KernelConsoleReadError {
    pub fn as_str(&self) -> &str {
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
    pub struct KernelConsoleReadFlags: u32 {
        const NONBLOCKING = 1;
    }
}

pub fn sys_kernel_console_read(
    _buffer: &mut [u8],
    _flags: KernelConsoleReadFlags,
) -> Result<usize, KernelConsoleReadError> {
    todo!()
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum KernelConsoleReadBufferError {
    WouldBlock,
}

impl KernelConsoleReadBufferError {
    pub fn as_str(&self) -> &str {
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
    pub struct KernelConsoleReadBufferFlags: u32 {
        const NONBLOCKING = 1;
    }
}

pub fn sys_kernel_console_read_buffer(
    _buffer: &mut [u8],
    _flags: KernelConsoleReadBufferFlags,
) -> Result<usize, KernelConsoleReadBufferError> {
    todo!()
}

bitflags! {
    pub struct KernelConsoleWriteFlags: u32 {
        const DISCARD_ON_FULL = 1;
    }
}

pub fn sys_kernel_console_write(
    _buffer: &[u8],
    _flags: KernelConsoleWriteFlags,
) -> Result<usize, ()> {
    todo!()
}
