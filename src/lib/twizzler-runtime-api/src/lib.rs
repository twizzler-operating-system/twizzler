#![no_std]

use core::{
    alloc::GlobalAlloc, ffi::CStr, fmt::Display, num::NonZeroUsize, sync::atomic::AtomicU32,
    time::Duration,
};

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
    fn get_runtime<'a>() -> &'a Self;
    // todo: get random
}

/// Arguments that std expects to pass to spawn.
pub struct ThreadSpawnArgs {
    /// The initial stack size
    pub stack_size: usize,
    /// The entry point
    pub start: usize,
    /// The argument to the entry point
    pub arg: usize,
}

/// All the thread-related runtime
pub trait ThreadRuntime {
    /// What the runtime calls threads.
    type InternalId: Copy;
    /// Possible errors from spawn.
    type SpawnError: InternalError;

    /// How big rust should make the initial stack
    const DEFAULT_MIN_STACK_SIZE: usize;

    /// Essentially number of threads on this system
    fn available_parallelism(&self) -> NonZeroUsize;

    /// Wait for futex (see: Linux)
    fn futex_wait(&self, futex: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool;
    /// Wake one for futex (see: Linux)
    fn futex_wake(&self, futex: &AtomicU32) -> bool;
    /// Wake all for futex (see: Linux)
    fn futex_wake_all(&self, futex: &AtomicU32);

    /// Spawn a thread
    fn spawn(&self, args: ThreadSpawnArgs) -> Result<Self::InternalId, Self::SpawnError>;

    /// Yield calling thread
    fn yield_now(&self);

    /// Set the name of calling thread
    fn set_name(&self, name: &CStr);

    /// Sleep calling thread
    fn sleep(&self, duration: Duration);

    /// Specified thread is joining
    fn join(&self, id: Self::InternalId);
}

pub trait ObjectRuntime {}

pub trait CoreRuntime {
    type AllocatorType: GlobalAlloc;
    /// Return an allocator for default allocations
    fn default_allocator(&self) -> &Self::AllocatorType;

    /// Called by std before calling main
    fn pre_main_hook(&self) {}

    /// Called by std after returning from main
    fn post_main_hook(&self) {}

    /// Exit, directly invoked by user
    fn exit(&self, code: i32) -> !;

    /// Thread abort
    fn abort(&self) -> !;
}

/// Arguments passed by the runtime to libstd.
pub struct BasicAux {
    pub argc: usize,
    pub args: *const *const i8,
    pub env: *const *const i8,
}

/// Return value returned by std from LibStdEntry
pub struct BasicReturn {}

/// Runtime that implements std's FS support
pub trait RustFsRuntime {}

/// Runtime that implements std's process and command support
pub trait RustProcessRuntime: RustStdioRuntime {}

/// Runtime that implements stdio
pub trait RustStdioRuntime {
    type Stdin: IoRead;
    type Stdout: IoWrite;
    type Stderr: IoWrite;
    type PanicOutput: IoWrite;

    /// Get a writable object for panic writes.
    fn panic_output(&self) -> Self::PanicOutput;
}

/// Trait for stdin
pub trait IoRead {
    type ReadErrorType: InternalError;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::ReadErrorType>;
}

/// Trait for stdout/stderr
pub trait IoWrite {
    type WriteErrorType: InternalError;
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::WriteErrorType>;
    fn flush(&mut self) -> Result<(), Self::WriteErrorType>;
}

/// Runtime trait for std's time support
pub trait RustTimeRuntime {
    type InstantType: RustInstant + Into<Duration>;
    type SystemTimeType: Into<Duration>;

    fn get_monotonic(&self) -> Self::InstantType;
    fn get_system_time(&self) -> Self::SystemTimeType;
}

/// Additional helper functions for Instant
pub trait RustInstant {
    fn actually_monotonic(&self) -> bool;
}

/// This is the type of the function exposed by std that the runtime calls to transfer control to the Rust std + main.
pub type LibstdEntry = fn(aux: BasicAux) -> BasicReturn;

/// Error types
pub trait InternalError: core::fmt::Debug + Display {}

pub trait DebugRuntime {
    type LibType: Library;
    type LibIterator: core::iter::Iterator<Item = Self::LibType>;
    fn iter_libs(&self) -> Self::LibIterator;
}

pub trait Library {}
