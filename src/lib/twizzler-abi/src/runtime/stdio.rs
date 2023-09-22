use core::fmt::Debug;

use twizzler_runtime_api::{IoRead, IoWrite, ReadError, RustStdioRuntime, WriteError};

use crate::syscall::KernelConsoleReadError;

use super::MinimalRuntime;

impl RustStdioRuntime for MinimalRuntime {
    fn with_panic_output(&self, cb: twizzler_runtime_api::IoWriteDynCallback<'_, ()>) {
        let mut wp = WritePoint {};
        cb(&mut wp);
    }

    fn with_stdin(
        &self,
        cb: twizzler_runtime_api::IoReadDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::ReadError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::ReadError> {
        let mut rp = ReadPoint {};
        cb(&mut rp)
    }

    fn with_stdout(
        &self,
        cb: twizzler_runtime_api::IoWriteDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::WriteError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::WriteError> {
        let mut wp = WritePoint {};
        cb(&mut wp)
    }

    fn with_stderr(
        &self,
        cb: twizzler_runtime_api::IoWriteDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::WriteError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::WriteError> {
        let mut wp = WritePoint {};
        cb(&mut wp)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
pub struct WritePoint {}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
pub struct ReadPoint {}

impl IoWrite for WritePoint {
    fn write(&mut self, buf: &[u8]) -> Result<usize, WriteError> {
        crate::syscall::sys_kernel_console_write(
            buf,
            crate::syscall::KernelConsoleWriteFlags::empty(),
        );
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), WriteError> {
        Ok(())
    }
}

impl IoRead for ReadPoint {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ReadError> {
        crate::syscall::sys_kernel_console_read(
            buf,
            crate::syscall::KernelConsoleReadFlags::empty(),
        )
        .map_err(|e| e.into())
    }
}

impl From<KernelConsoleReadError> for ReadError {
    fn from(_: KernelConsoleReadError) -> Self {
        todo!()
    }
}
