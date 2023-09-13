use core::fmt::Debug;

use twizzler_runtime_api::{InternalError, IoRead, IoWrite, RustStdioRuntime};

use crate::syscall::KernelConsoleReadError;

use super::MinimalRuntime;

impl RustStdioRuntime for MinimalRuntime {
    type Stdin = ReadPoint;

    type Stdout = WritePoint;

    type Stderr = WritePoint;

    type PanicOutput = WritePoint;

    fn panic_output(&self) -> Self::PanicOutput {
        WritePoint {}
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
pub struct WritePoint {}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
pub struct ReadPoint {}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub enum WriteError {}

impl core::fmt::Display for WriteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        <Self as Debug>::fmt(&self, f)
    }
}

impl InternalError for WriteError {}

impl IoWrite for WritePoint {
    type WriteErrorType = WriteError;

    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::WriteErrorType> {
        crate::syscall::sys_kernel_console_write(
            buf,
            crate::syscall::KernelConsoleWriteFlags::empty(),
        );
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Self::WriteErrorType> {
        Ok(())
    }
}

impl IoRead for ReadPoint {
    type ReadErrorType = KernelConsoleReadError;

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::ReadErrorType> {
        crate::syscall::sys_kernel_console_read(
            buf,
            crate::syscall::KernelConsoleReadFlags::empty(),
        )
    }
}

impl InternalError for KernelConsoleReadError {}
