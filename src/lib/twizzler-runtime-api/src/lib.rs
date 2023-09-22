#![no_std]
#![feature(unboxed_closures)]
#![feature(naked_functions)]

use core::{
    alloc::GlobalAlloc, ffi::CStr, num::NonZeroUsize, sync::atomic::AtomicU32, time::Duration,
};

#[cfg(all(
    feature = "runtime",
    not(feature = "kernel"),
    feature = "rustc-dep-of-std"
))]
pub mod rt0;

pub type ObjID = u128;

#[repr(C)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
/// Auxillary information provided to a new program on runtime entry.
pub enum AuxEntry {
    /// Ends the aux array.
    Null,
    /// A pointer to this program's program headers, and the number of them. See the ELF
    /// specification for more info.
    ProgramHeaders(u64, usize),
    /// A pointer to the env var array.
    Environment(u64),
    /// A pointer to the arguments array.
    Arguments(usize, u64),
    /// The object ID of the executable.
    ExecId(ObjID),
}

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

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub struct TlsIndex {
    pub mod_id: usize,
    pub offset: usize,
}

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
    fn spawn(&self, args: ThreadSpawnArgs) -> Result<u32, SpawnError>;

    /// Yield calling thread
    fn yield_now(&self);

    /// Set the name of calling thread
    fn set_name(&self, name: &CStr);

    /// Sleep calling thread
    fn sleep(&self, duration: Duration);

    /// Specified thread is joining
    fn join(&self, id: u32, timeout: Option<Duration>) -> Result<(), JoinError>;

    fn tls_get_addr(&self, tls_index: &TlsIndex) -> *const u8;
}

pub trait ObjectRuntime {
    fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle, MapError>;
    fn unmap_object(&self, handle: &ObjectHandle);
    fn release_handle(&self, handle: &mut ObjectHandle);
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum JoinError {
    LookupError,
    Timeout,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum MapError {
    Unknown,
    InternalError,
    OutOfMemory,
    NoSuchObject,
    PermissionDenied,
    InvalidArgument,
}

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

#[cfg(not(feature = "kernel"))]
impl Drop for ObjectHandle {
    fn drop(&mut self) {
        let runtime = get_runtime();
        runtime.release_handle(self);
    }
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

    fn runtime_entry(
        &self,
        arg: *const AuxEntry,
        std_entry: unsafe extern "C" fn(BasicAux) -> BasicReturn,
    ) -> !;
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// Arguments passed by the runtime to libstd.
pub struct BasicAux {
    pub argc: usize,
    pub args: *const *const i8,
    pub env: *const *const i8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// Return value returned by std from LibStdEntry
pub struct BasicReturn {
    pub code: i32,
}

/// Runtime that implements std's FS support
pub trait RustFsRuntime {}

/// Runtime that implements std's process and command support
pub trait RustProcessRuntime: RustStdioRuntime {}

pub type IoReadDynCallback<'a, R> = &'a mut (dyn (FnMut(&mut dyn IoRead) -> R));

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

pub struct Library {
    pub mapping: ObjectHandle,
    pub range: (*const u8, *const u8),
}

impl AsRef<Library> for Library {
    fn as_ref(&self) -> &Library {
        self
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LibraryId(pub usize);

unsafe impl Send for Library {}

/// Error types
pub trait DebugRuntime {
    fn get_library(&self, id: LibraryId) -> Option<Library>;
    fn get_exeid(&self) -> Option<LibraryId>;
    fn get_library_segment(&self, lib: &Library, seg: usize) -> Option<AddrRange>;
    fn get_full_mapping(&self, lib: &Library) -> Option<ObjectHandle>;
}

pub struct AddrRange {
    pub start: usize,
    pub len: usize,
}

#[cfg(not(feature = "kernel"))]
extern "rust-call" {
    fn __twz_get_runtime(_a: ()) -> &'static (dyn Runtime + Sync);
}

#[cfg(not(feature = "kernel"))]
pub fn get_runtime() -> &'static (dyn Runtime + Sync) {
    unsafe { __twz_get_runtime(()) }
}
