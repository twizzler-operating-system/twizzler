use twizzler_abi::syscall::{
    sys_kernel_console_read, sys_kernel_console_write, KernelConsoleReadFlags,
    KernelConsoleWriteFlags,
};
use twizzler_runtime_api::{IoRead, IoWrite, ReadError, RustStdioRuntime};

use super::ReferenceRuntime;

impl RustStdioRuntime for ReferenceRuntime {
    fn with_panic_output(&self, cb: twizzler_runtime_api::IoWriteDynCallback<'_, ()>) {
        cb(&mut IoWritePoint {})
    }

    fn with_stdin(
        &self,
        cb: twizzler_runtime_api::IoReadDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::ReadError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::ReadError> {
        cb(&mut IoReadPoint {})
    }

    fn with_stdout(
        &self,
        cb: twizzler_runtime_api::IoWriteDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::WriteError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::WriteError> {
        cb(&mut IoWritePoint {})
    }

    fn with_stderr(
        &self,
        cb: twizzler_runtime_api::IoWriteDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::WriteError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::WriteError> {
        cb(&mut IoWritePoint {})
    }
}

struct IoWritePoint {}

impl IoWrite for IoWritePoint {
    fn write(&mut self, buf: &[u8]) -> Result<usize, twizzler_runtime_api::WriteError> {
        sys_kernel_console_write(buf, KernelConsoleWriteFlags::empty());
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), twizzler_runtime_api::WriteError> {
        Ok(())
    }
}

struct IoReadPoint {}

impl IoRead for IoReadPoint {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, twizzler_runtime_api::ReadError> {
        let len = sys_kernel_console_read(buf, KernelConsoleReadFlags::empty())
            .map_err(|_| ReadError::IoError)?;
        Ok(len)
    }
}
