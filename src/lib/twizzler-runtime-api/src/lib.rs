#![no_std]

use core::{
    alloc::GlobalAlloc, ffi::CStr, fmt::Display, num::NonZeroUsize, sync::atomic::AtomicU32,
    time::Duration,
};

pub type ObjID = u128;

/// Full runtime trait, composed of smaller traits
pub trait Runtime:
    ThreadRuntime
    + ObjectRuntime
    + CoreRuntime
    + RustFsRuntime
    + RustProcessRuntime
    + RustStdioRuntime
    + DebugRuntime
    + RustTimeRuntime
{
    // todo: get random
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// Arguments that std expects to pass to spawn.
pub struct ThreadSpawnArgs {
    /// The initial stack size
    pub stack_size: usize,
    /// The entry point
    pub start: usize,
    /// The argument to the entry point
    pub arg: usize,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum SpawnError {}

/// All the thread-related runtime
pub trait ThreadRuntime {
    /// Essentially number of threads on this system
    fn available_parallelism(&self) -> NonZeroUsize;

    /// Wait for futex (see: Linux)
    fn futex_wait(&self, futex: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool;
    /// Wake one for futex (see: Linux)
    fn futex_wake(&self, futex: &AtomicU32) -> bool;
    /// Wake all for futex (see: Linux)
    fn futex_wake_all(&self, futex: &AtomicU32);

    /// Spawn a thread
    fn spawn(&self, args: ThreadSpawnArgs) -> Result<ObjID, SpawnError>;

    /// Yield calling thread
    fn yield_now(&self);

    /// Set the name of calling thread
    fn set_name(&self, name: &CStr);

    /// Sleep calling thread
    fn sleep(&self, duration: Duration);

    /// Specified thread is joining
    fn join(&self, id: ObjID, timeout: Option<Duration>) -> Result<(), JoinError>;
}

pub trait ObjectRuntime {
    fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle, MapError>;
    fn unmap_object(&self, handle: ObjectHandle);
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum JoinError {
    LookupError,
    Timeout,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum MapError {}

bitflags::bitflags! {
    /// Mapping protections for mapping objects into the address space.
    pub struct MapFlags: u32 {
        /// Read allowed.
        const READ = 1;
        /// Write allowed.
        const WRITE = 2;
        /// Exec allowed.
        const EXEC = 4;
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub struct ObjectHandle {
    pub id: ObjID,
    pub flags: MapFlags,
    pub base: *mut u8,
}

pub trait CoreRuntime {
    fn default_allocator(&self) -> &'static dyn GlobalAlloc;

    /// Called by std before calling main
    fn pre_main_hook(&self) {}

    /// Called by std after returning from main
    fn post_main_hook(&self) {}

    /// Exit, directly invoked by user
    fn exit(&self, code: i32) -> !;

    /// Thread abort
    fn abort(&self) -> !;
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// Arguments passed by the runtime to libstd.
pub struct BasicAux {
    pub argc: usize,
    pub args: *const *const i8,
    pub env: *const *const i8,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// Return value returned by std from LibStdEntry
pub struct BasicReturn {}

/// Runtime that implements std's FS support
pub trait RustFsRuntime {}

/// Runtime that implements std's process and command support
pub trait RustProcessRuntime: RustStdioRuntime {}

pub type IoReadDynCallback<'a, R> = &'a (dyn (Fn(&mut dyn IoRead) -> R));

pub type IoWriteDynCallback<'a, R> = &'a (dyn (Fn(&mut dyn IoWrite) -> R));

/// Runtime that implements stdio
pub trait RustStdioRuntime {
    /// Get a writable object for panic writes.
    fn with_panic_output(&self, cb: IoWriteDynCallback<'_, ()>);

    fn with_stdin(
        &self,
        cb: IoReadDynCallback<'_, Result<usize, ReadError>>,
    ) -> Result<usize, ReadError>;

    fn with_stdout(
        &self,
        cb: IoWriteDynCallback<'_, Result<usize, WriteError>>,
    ) -> Result<usize, WriteError>;

    fn with_stderr(
        &self,
        cb: IoWriteDynCallback<'_, Result<usize, WriteError>>,
    ) -> Result<usize, WriteError>;
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum ReadError {}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum WriteError {}

/// Trait for stdin
pub trait IoRead {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, ReadError>;
}

/// Trait for stdout/stderr
pub trait IoWrite {
    fn write(&mut self, buf: &[u8]) -> Result<usize, WriteError>;
    fn flush(&mut self) -> Result<(), WriteError>;
}

/// Runtime trait for std's time support
pub trait RustTimeRuntime {
    fn get_monotonic(&self) -> Duration;
    fn get_system_time(&self) -> Duration;
    fn actually_monotonic(&self) -> bool;
}

/// This is the type of the function exposed by std that the runtime calls to transfer control to the Rust std + main.
pub type LibstdEntry = fn(aux: BasicAux) -> BasicReturn;

/// Error types
pub trait DebugRuntime {
    fn iter_libs(&self) -> ();
}
