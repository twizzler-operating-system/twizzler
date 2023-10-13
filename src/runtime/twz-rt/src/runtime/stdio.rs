use std::{
    panic::RefUnwindSafe,
    sync::{Arc, RwLock},
};

use twizzler_abi::syscall::{
    sys_kernel_console_read, sys_kernel_console_write, KernelConsoleReadFlags,
    KernelConsoleWriteFlags,
};
use twizzler_runtime_api::{IoRead, IoWrite, ReadError, RustStdioRuntime};

use super::ReferenceRuntime;

#[thread_local]
static THREAD_STDIN: RwLock<Option<Arc<dyn IoRead>>> = RwLock::new(None);
#[thread_local]
static THREAD_STDOUT: RwLock<Option<Arc<dyn IoWrite>>> = RwLock::new(None);
#[thread_local]
static THREAD_STDERR: RwLock<Option<Arc<dyn IoWrite + RefUnwindSafe>>> = RwLock::new(None);

static LOCAL_STDIN: RwLock<Option<Arc<dyn IoRead + Sync + Send>>> = RwLock::new(None);
static LOCAL_STDOUT: RwLock<Option<Arc<dyn IoWrite + Sync + Send>>> = RwLock::new(None);
static LOCAL_STDERR: RwLock<Option<Arc<dyn IoWrite + Sync + Send + RefUnwindSafe>>> =
    RwLock::new(None);

// TODO: configure fallbacks

#[allow(dead_code)]
impl ReferenceRuntime {
    pub fn set_stdin(&self, thread_local: bool, stream: Arc<dyn IoRead + Sync + Send>) {
        // Unwrap-Ok: we don't panic when holding the write lock.
        if thread_local {
            *THREAD_STDIN.write().unwrap() = Some(stream);
        } else {
            *LOCAL_STDIN.write().unwrap() = Some(stream);
        }
    }

    pub fn set_stdout(&self, thread_local: bool, stream: Arc<dyn IoWrite + Sync + Send>) {
        // Unwrap-Ok: we don't panic when holding the write lock.
        if thread_local {
            *THREAD_STDOUT.write().unwrap() = Some(stream);
        } else {
            *LOCAL_STDOUT.write().unwrap() = Some(stream);
        }
    }

    pub fn set_stderr(
        &self,
        thread_local: bool,
        stream: Arc<dyn IoWrite + Sync + Send + RefUnwindSafe>,
    ) {
        // Unwrap-Ok: we don't panic when holding the write lock.
        if thread_local {
            *THREAD_STDERR.write().unwrap() = Some(stream);
        } else {
            *LOCAL_STDERR.write().unwrap() = Some(stream);
        }
    }
}

impl RustStdioRuntime for ReferenceRuntime {
    fn with_panic_output(&self, cb: twizzler_runtime_api::IoWritePanicDynCallback<'_, ()>) {
        // For panic output, try to never wait on any locks. Also, catch unwinds and treat
        // the output option as None if the callback panics, to try to ensure the output goes somewhere.

        // Unwrap-Ok: we ensure that no one can panic when holding the read lock.
        if let Ok(ref out) = THREAD_STDERR.try_read() {
            if let Some(ref out) = **out {
                if std::panic::catch_unwind(|| cb(&**out)).is_ok() {
                    return;
                }
            }
        }

        if let Ok(ref out) = LOCAL_STDERR.try_read() {
            if let Some(ref out) = **out {
                if std::panic::catch_unwind(|| cb(&**out)).is_ok() {
                    return;
                }
            }
        }

        // We've done all we can do.
        let _ = std::panic::catch_unwind(|| cb(&mut FallbackWritePoint {}));
    }

    fn with_stdin(
        &self,
        cb: twizzler_runtime_api::IoReadDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::ReadError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::ReadError> {
        // Unwrap-Ok: we ensure that no one can panic when holding the read lock.
        if let Some(ref out) = &*THREAD_STDIN.read().unwrap() {
            // TODO: do we need to catch unwinds here?
            return cb(&**out);
        }

        if let Some(ref out) = &*LOCAL_STDIN.read().unwrap() {
            // TODO: do we need to catch unwinds here?
            return cb(&**out);
        }

        // TODO: do we need to catch unwinds here?
        cb(&mut FallbackReadPoint {})
    }

    fn with_stdout(
        &self,
        cb: twizzler_runtime_api::IoWriteDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::WriteError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::WriteError> {
        // Unwrap-Ok: we ensure that no one can panic when holding the read lock.
        if let Some(ref out) = &*THREAD_STDOUT.read().unwrap() {
            // TODO: do we need to catch unwinds here?
            return cb(&**out);
        }

        if let Some(ref out) = &*LOCAL_STDOUT.read().unwrap() {
            // TODO: do we need to catch unwinds here?
            return cb(&**out);
        }

        // TODO: do we need to catch unwinds here?
        cb(&mut FallbackWritePoint {})
    }

    fn with_stderr(
        &self,
        cb: twizzler_runtime_api::IoWriteDynCallback<
            '_,
            Result<usize, twizzler_runtime_api::WriteError>,
        >,
    ) -> Result<usize, twizzler_runtime_api::WriteError> {
        // Unwrap-Ok: we ensure that no one can panic when holding the read lock.
        if let Some(ref out) = &*THREAD_STDERR.read().unwrap() {
            // TODO: do we need to catch unwinds here?
            return cb(&**out);
        }

        if let Some(ref out) = &*LOCAL_STDERR.read().unwrap() {
            // TODO: do we need to catch unwinds here?
            return cb(&**out);
        }

        // TODO: do we need to catch unwinds here?
        cb(&mut FallbackWritePoint {})
    }
}

struct FallbackWritePoint {}

impl IoWrite for FallbackWritePoint {
    fn write(&self, buf: &[u8]) -> Result<usize, twizzler_runtime_api::WriteError> {
        sys_kernel_console_write(buf, KernelConsoleWriteFlags::empty());
        Ok(buf.len())
    }

    fn flush(&self) -> Result<(), twizzler_runtime_api::WriteError> {
        Ok(())
    }
}

struct FallbackReadPoint {}

impl IoRead for FallbackReadPoint {
    fn read(&self, buf: &mut [u8]) -> Result<usize, twizzler_runtime_api::ReadError> {
        let len = sys_kernel_console_read(buf, KernelConsoleReadFlags::empty())
            .map_err(|_| ReadError::IoError)?;
        Ok(len)
    }
}
