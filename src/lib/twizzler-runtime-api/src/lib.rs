//! The Twizzler Runtime API is the core interface definition for Twizzler programs, including startup, execution, and libstd support.
//! It defines a set of traits that, when all implemented, form the full interface that Rust's libstd expects from a Twizzler runtime.
//!
//! From a high level, a Twizzler program links against Rust's libstd and a particular runtime that will support libstd. That runtime
//! must implement the minimum set of interfaces required by the [Runtime] trait. Libstd then invokes the runtime functions when needed
//! (e.g. allocating memory, exiting a thread, etc.). Other libraries may invoke runtime functions directly as well (bypassing libstd),
//! but note that doing so may not play nicely with libstd's view of the world.
//!
//! # What does it look like to use the runtime?
//!
//! When a program (including libstd) wishes to use the runtime, it invokes this library's [get_runtime] function, which will return
//! a reference (a &'static dyn reference) to a type that implements the Runtime trait. From there, runtime functions can be called:
//! ```
//! let runtime = get_runtime();
//! runtime.get_monotonic()
//! ```
//! Note that this function is only exposed if the runtime feature is enabled.
//!
//! # So who is providing that type that implements [Runtime]?
//!
//! Another library! Right now, Twizzler defines two runtimes: a "minimal" runtime, and a "reference" runtime. Those are not implemented
//! in this crate. The minimal runtime is implemented as part of the twizzler-abi crate, as it's the most "baremetal" runtime. The
//! reference runtime is implemented as a standalone set of crates. Of course, other runtimes can be implemented, as long as they implement
//! the required interface in this crate, libstd will work.
//!
//! ## Okay but how does get_runtime work?
//!
//! Well, [get_runtime] is just a wrapper around calling an extern "C" function, [__twz_get_runtime]. This symbol is external, so not
//! defined in this crate. A crate that implements [Runtime] then defines [__twz_get_runtime], allowing link-time swapping of runtimes.
//! The twizzler-abi crate defines this symbol with (weak linkage)[https://en.wikipedia.org/wiki/Weak_symbol], causing it to be linked
//! only if another (strong) definition is not present. Thus, a program can link to a specific runtime, but it can also be loaded by a
//! dynamic linker and have its runtime selected at load time.

#![no_std]
#![feature(unboxed_closures)]
#![feature(naked_functions)]

use core::{
    alloc::GlobalAlloc, ffi::CStr, num::NonZeroUsize, panic::RefUnwindSafe,
    sync::atomic::AtomicU32, time::Duration,
};

#[cfg(feature = "rt0")]
pub mod rt0;

/// Core object ID type in Twizzler.
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
    /// Initial runtime information. The value is runtime-specific.
    RuntimeInfo(usize),
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

/// Possible errors on spawn.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum SpawnError {
    /// An error that is not classified.
    Other,
    /// One of the arguments in spawn args was invalid.
    InvalidArgument,
    /// An object used as a handle was not found.
    ObjectNotFound,
    /// An object used as a handle may not be accessed by the caller.
    PermissionDenied,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// An ABI-defined argument passed to __tls_get_addr.
pub struct TlsIndex {
    /// The ID of the module.
    pub mod_id: usize,
    /// The offset into that module's TLS region.
    pub offset: usize,
}

/// All the thread-related runtime functions.
pub trait ThreadRuntime {
    /// Essentially number of threads on this system
    fn available_parallelism(&self) -> NonZeroUsize;

    /// Wait for futex (see: Linux)
    fn futex_wait(&self, futex: &AtomicU32, expected: u32, timeout: Option<Duration>) -> bool;
    /// Wake one for futex (see: Linux)
    fn futex_wake(&self, futex: &AtomicU32) -> bool;
    /// Wake all for futex (see: Linux)
    fn futex_wake_all(&self, futex: &AtomicU32);

    /// Spawn a thread, returning an internal ID that uniquely identifies a thread in the runtime.
    fn spawn(&self, args: ThreadSpawnArgs) -> Result<u32, SpawnError>;

    /// Yield calling thread
    fn yield_now(&self);

    /// Set the name of calling thread
    fn set_name(&self, name: &CStr);

    /// Sleep calling thread
    fn sleep(&self, duration: Duration);

    /// Wait for the specified thread to terminate, or optionally time out.
    fn join(&self, id: u32, timeout: Option<Duration>) -> Result<(), JoinError>;

    /// Implements the __tls_get_addr functionality. If the runtime feature is enabled, this crate defines the
    /// extern "C" function __tls_get_addr as a wrapper around calling this function after getting the runtime from [get_runtime].
    /// If the provided index is invalid, return None.
    fn tls_get_addr(&self, tls_index: &TlsIndex) -> Option<*const u8>;
}

/// All the object related runtime functions.
pub trait ObjectRuntime {
    /// Map an object to an [ObjectHandle]. The handle may reference the same internal mapping as other calls to this function.
    fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle, MapError>;
    /// Unmap an object handle.
    fn unmap_object(&self, handle: &ObjectHandle);
    /// Called on drop of an object handle.
    fn release_handle(&self, handle: &mut ObjectHandle);
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// Possible errors of join.
pub enum JoinError {
    /// The internal-thread-ID does not exist.
    LookupError,
    /// Join timed out.
    Timeout,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// Possible errors of mapping an object.
pub enum MapError {
    /// Error is unclassified.
    Other,
    /// An internal runtime error occurred.
    InternalError,
    /// Ran out of resources when trying to map the object.
    OutOfResources,
    /// The specified object does not exist.
    NoSuchObject,
    /// Access is disallowed.
    PermissionDenied,
    /// An argument to map is invalid.
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
/// A handle to an internal object.
pub struct ObjectHandle {
    /// The ID of the object.
    pub id: ObjID,
    /// The flags of this handle.
    pub flags: MapFlags,
    /// A pointer to the object's start (null-page, not base).
    pub start: *mut u8,
    /// A pointer to the object's metadata.
    pub meta: *mut u8,
}

#[cfg(not(feature = "kernel"))]
impl Drop for ObjectHandle {
    fn drop(&mut self) {
        let runtime = get_runtime();
        runtime.release_handle(self);
    }
}

/// Definitions of core runtime features.
pub trait CoreRuntime {
    /// Returns a reference to an allocator to use for default (global) allocations.
    fn default_allocator(&self) -> &'static dyn GlobalAlloc;

    /// Called by libstd before calling main.
    fn pre_main_hook(&self) {}

    /// Called by libstd after returning from main.
    fn post_main_hook(&self) {}

    /// Exit the calling thread. This is allowed to cause a full exit of the entire program and all threads.
    fn exit(&self, code: i32) -> !;

    /// Thread abort. This is allowed to cause a full exit of the entire program and all threads.
    fn abort(&self) -> !;

    /// Called by rt0 code to start the runtime. Once the runtime has initialized, it should call the provided entry function. The pointer
    /// arg is a pointer to an array of [AuxEntry] that terminates with an [AuxEntry::Null].
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
    /// The number of arguments.
    pub argc: usize,
    /// A null-terminated list of null-terminated strings, forming arguments to the program.
    pub args: *const *const i8,
    /// The environment pointer, also a null-terminated list of null-terminated strings.
    pub env: *const *const i8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
/// Return value returned by std from LibStdEntry
pub struct BasicReturn {
    /// Exit code. 0 is success, non-zero is application-defined.
    pub code: i32,
}

/// Runtime that implements std's FS support. Currently unimplemented.
pub trait RustFsRuntime {}

/// Runtime that implements std's process and command support. Currently unimplemented.
pub trait RustProcessRuntime: RustStdioRuntime {}

/// The type of a callback to an IO Read call (see: [RustStdioRuntime]).
pub type IoReadDynCallback<'a, R> = &'a mut (dyn (FnMut(&dyn IoRead) -> R));

/// The type of a callback to an IO Write call (see: [RustStdioRuntime]).
pub type IoWriteDynCallback<'a, R> = &'a (dyn (Fn(&dyn IoWrite) -> R));

/// The type of a callback to an IO Write call (see: [RustStdioRuntime]).
pub type IoWritePanicDynCallback<'a, R> = &'a (dyn (Fn(&dyn IoWrite) -> R) + RefUnwindSafe);

/// Runtime that implements stdio.
pub trait RustStdioRuntime {
    /// Execute a closure with an implementer of [IoWrite] that can be used for panic output.
    fn with_panic_output(&self, cb: IoWritePanicDynCallback<'_, ()>);

    /// Execute a closure with an implementer of [IoRead] that can be used for stdin.
    fn with_stdin(
        &self,
        cb: IoReadDynCallback<'_, Result<usize, ReadError>>,
    ) -> Result<usize, ReadError>;

    /// Execute a closure with an implementer of [IoWrite] that can be used for stdout.
    fn with_stdout(
        &self,
        cb: IoWriteDynCallback<'_, Result<usize, WriteError>>,
    ) -> Result<usize, WriteError>;

    /// Execute a closure with an implementer of [IoWrite] that can be used for stderr.
    fn with_stderr(
        &self,
        cb: IoWriteDynCallback<'_, Result<usize, WriteError>>,
    ) -> Result<usize, WriteError>;
}

/// Possible errors from read.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum ReadError {
    /// Unclassified error
    Other,
    /// IO Error
    IoError,
    /// Permission denied
    PermissionDenied,
    /// No such IO mechanism.
    NoIo,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum WriteError {
    /// Unclassified error
    Other,
    /// IO Error
    IoError,
    /// Permission denied
    PermissionDenied,
    /// No such IO mechanism.
    NoIo,
}

/// Trait for stdin
pub trait IoRead {
    /// Read data into buf, returning the number of bytes read.
    fn read(&self, buf: &mut [u8]) -> Result<usize, ReadError>;
}

/// Trait for stdout/stderr
pub trait IoWrite {
    /// Write data from buf, returning the number of bytes written.
    fn write(&self, buf: &[u8]) -> Result<usize, WriteError>;
    /// Flush any buffered internal data. This function is allowed to be a no-op.
    fn flush(&self) -> Result<(), WriteError>;
}

/// Runtime trait for libstd's time support
pub trait RustTimeRuntime {
    /// Get a monotonic timestamp.
    fn get_monotonic(&self) -> Duration;
    /// Get a system time timestamp.
    fn get_system_time(&self) -> Duration;
    /// Is the monotonic timestamp monotonic or not?
    fn actual_monotonicity(&self) -> Monotonicity;
}

/// Possible types of monotonicity.
pub enum Monotonicity {
    /// Not monotonic at all.
    NonMonotonic,
    /// Weakly monotonic (function may increase or stay the same).
    Weak,
    /// Strictly monotonic (function always increases).
    Strict,
}

/// An abstract representation of a library, useful for debugging and backtracing.
pub struct Library {
    /// How this library is mapped.
    pub mapping: ObjectHandle,
    /// Actual range of addresses that comprise the library binary data.
    pub range: (*const u8, *const u8),
}

impl AsRef<Library> for Library {
    fn as_ref(&self) -> &Library {
        self
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Internal library ID type.
pub struct LibraryId(pub usize);

/// The runtime must ensure that the addresses are constant for the whole life of the library type, and that all threads
/// may see the type.
unsafe impl Send for Library {}

/// Functions for the debug support part of libstd (e.g. unwinding, backtracing).
pub trait DebugRuntime {
    /// Gets a handle to a library given the ID.
    fn get_library(&self, id: LibraryId) -> Option<Library>;
    /// Returns the ID of the main executable, if there is one.
    fn get_exeid(&self) -> Option<LibraryId>;
    /// Get a segment of a library, if the segment index exists. All segment IDs are indexes, so they range from [0, N).
    fn get_library_segment(&self, lib: &Library, seg: usize) -> Option<AddrRange>;
    /// Get the full mapping of the underlying library.
    fn get_full_mapping(&self, lib: &Library) -> Option<ObjectHandle>;
}

/// An address range.
pub struct AddrRange {
    /// Starting virtual address.
    pub start: usize,
    /// Length of the range.
    pub len: usize,
}

#[cfg(not(feature = "kernel"))]
extern "rust-call" {
    /// Called by get_runtime to actually get the runtime.
    fn __twz_get_runtime(_a: ()) -> &'static (dyn Runtime + Sync);
}

#[cfg(not(feature = "kernel"))]
/// Wrapper around call to __twz_get_runtime.
pub fn get_runtime() -> &'static (dyn Runtime + Sync) {
    unsafe { __twz_get_runtime(()) }
}
