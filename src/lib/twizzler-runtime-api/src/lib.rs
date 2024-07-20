//! The Twizzler Runtime API is the core interface definition for Twizzler programs, including
//! startup, execution, and libstd support. It defines a set of traits that, when all implemented,
//! form the full interface that Rust's libstd expects from a Twizzler runtime.
//!
//! From a high level, a Twizzler program links against Rust's libstd and a particular runtime that
//! will support libstd. That runtime must implement the minimum set of interfaces required by the
//! [Runtime] trait. Libstd then invokes the runtime functions when needed (e.g. allocating memory,
//! exiting a thread, etc.). Other libraries may invoke runtime functions directly as well
//! (bypassing libstd), but note that doing so may not play nicely with libstd's view of the world.
//!
//! # What does it look like to use the runtime?
//!
//! When a program (including libstd) wishes to use the runtime, it invokes this library's
//! [get_runtime] function, which will return a reference (a &'static dyn reference) to a type that
//! implements the Runtime trait. From there, runtime functions can be called: ```
//! let runtime = get_runtime();
//! runtime.get_monotonic()
//! ```
//! Note that this function is only exposed if the runtime feature is enabled.
//!
//! # So who is providing that type that implements [Runtime]?
//!
//! Another library! Right now, Twizzler defines two runtimes: a "minimal" runtime, and a
//! "reference" runtime. Those are not implemented in this crate. The minimal runtime is implemented
//! as part of the twizzler-abi crate, as it's the most "baremetal" runtime. The reference runtime
//! is implemented as a standalone set of crates. Of course, other runtimes can be implemented, as
//! long as they implement the required interface in this crate, libstd will work.
//!
//! ## Okay but how does get_runtime work?
//!
//! Well, [get_runtime] is just a wrapper around calling an extern "C" function,
//! [__twz_get_runtime]. This symbol is external, so not defined in this crate. A crate that
//! implements [Runtime] then defines [__twz_get_runtime], allowing link-time swapping of runtimes. The twizzler-abi crate defines this symbol with (weak linkage)[https://en.wikipedia.org/wiki/Weak_symbol], causing it to be linked
//! only if another (strong) definition is not present. Thus, a program can link to a specific
//! runtime, but it can also be loaded by a dynamic linker and have its runtime selected at load
//! time.

#![no_std]
#![feature(unboxed_closures)]
#![feature(naked_functions)]
#![feature(c_size_t)]
#![feature(linkage)]
#![feature(core_intrinsics)]
#![feature(error_in_core)]

use core::fmt::{Display, LowerHex, UpperHex};
#[cfg_attr(feature = "kernel", allow(unused_imports))]
use core::{
    alloc::GlobalAlloc,
    ffi::CStr,
    num::NonZeroUsize,
    panic::RefUnwindSafe,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    time::Duration,
};

#[cfg(feature = "rt0")]
pub mod rt0;

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
/// An object ID, represented as a transparent wrapper type. Any value where the upper 64 bits are
/// zero is invalid.
pub struct ObjID(u128);

impl ObjID {
    /// Create a new ObjID out of a 128 bit value.
    pub const fn new(id: u128) -> Self {
        Self(id)
    }

    /// Split an object ID into upper and lower values, useful for syscalls.
    pub const fn split(&self) -> (u64, u64) {
        ((self.0 >> 64) as u64, (self.0 & 0xffffffffffffffff) as u64)
    }

    /// Build a new ObjID out of a high part and a low part.
    pub const fn new_from_parts(hi: u64, lo: u64) -> Self {
        ObjID::new(((hi as u128) << 64) | (lo as u128))
    }

    pub const fn as_u128(&self) -> u128 {
        self.0
    }
}

impl core::convert::AsRef<ObjID> for ObjID {
    fn as_ref(&self) -> &ObjID {
        self
    }
}

impl From<u128> for ObjID {
    fn from(id: u128) -> Self {
        Self::new(id)
    }
}

impl LowerHex for ObjID {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl UpperHex for ObjID {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:X}", self.0)
    }
}

impl core::fmt::Display for ObjID {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ObjID({:x})", self.0)
    }
}

impl core::fmt::Debug for ObjID {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ObjID({:x})", self.0)
    }
}

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
    RuntimeInfo(usize, u64),
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
    /// Failed to spawn thread in-kernel.
    KernelError,
}

impl Display for SpawnError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SpawnError::Other => write!(f, "unknown error"),
            SpawnError::InvalidArgument => write!(f, "invalid argument"),
            SpawnError::ObjectNotFound => write!(f, "object not found"),
            SpawnError::PermissionDenied => write!(f, "permission denied"),
            SpawnError::KernelError => write!(f, "kernel error"),
        }
    }
}

impl core::error::Error for SpawnError {}

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

    /// Implements the __tls_get_addr functionality. If the runtime feature is enabled, this crate
    /// defines the extern "C" function __tls_get_addr as a wrapper around calling this function
    /// after getting the runtime from [get_runtime]. If the provided index is invalid, return
    /// None.
    fn tls_get_addr(&self, tls_index: &TlsIndex) -> Option<*const u8>;
}

/// Possible errors on FOT resolve.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
pub enum FotResolveError {
    /// An error that is not classified.
    Other,
    /// Null pointer
    NullPointer,
    /// One of the arguments in FOT resolution call was invalid.
    InvalidArgument,
    /// FOT index is invalid.
    InvalidIndex,
    /// FOT entry at given index is invalid.
    InvalidFOTEntry,
    /// Mapping failed
    MapFailed(MapError),
}

impl From<MapError> for FotResolveError {
    fn from(value: MapError) -> Self {
        Self::MapFailed(value)
    }
}

impl Display for FotResolveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FotResolveError::Other => write!(f, "unknown error"),
            FotResolveError::NullPointer => write!(f, "null pointer"),
            FotResolveError::InvalidArgument => write!(f, "invalid argument"),
            FotResolveError::InvalidIndex => write!(f, "invalid index"),
            FotResolveError::InvalidFOTEntry => write!(f, "invalid FOT entry"),
            FotResolveError::MapFailed(me) => write!(f, "mapping failed: {}", me),
        }
    }
}

impl core::error::Error for FotResolveError {}

pub enum StartOrHandle {
    Start(*const u8),
    Handle(ObjectHandle),
}

/// All the object related runtime functions.
pub trait ObjectRuntime {
    /// Map an object to an [ObjectHandle]. The handle may reference the same internal mapping as
    /// other calls to this function.
    fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle, MapError>;
    /// Called on drop of an object handle.
    fn release_handle(&self, handle: &mut ObjectHandle);

    /// Given a pointer, return the object handle associated with that memory. Note that the
    /// returned handle may not necessarily point to the same virtual address as the pointer passed
    /// to this function. Also returns the offset from the start of the object where va points to.
    fn ptr_to_handle(&self, va: *const u8) -> Option<(ObjectHandle, usize)>;

    /// Given a pointer, return a pointer to the start of the associated object. Ensures that at
    /// least valid_len bytes after the returned pointer are valid to use. Also returns the offset
    /// from the start of the object where va points to.
    fn ptr_to_object_start(&self, va: *const u8, valid_len: usize) -> Option<(*const u8, usize)>;

    /// Resolve an object handle's FOT entry idx into a pointer to the start of the referenced
    /// object. Ensures that at least valid_len bytes after the returned pointer are valid to use.
    fn resolve_fot_to_object_start(
        &self,
        handle: &ObjectHandle,
        idx: usize,
        valid_len: usize,
    ) -> Result<StartOrHandle, FotResolveError>;

    /// Add an FOT entry to the object, returning a pointer to the FOT entry and the entry index. If
    /// there are no more FOT entries, or if the object is immutable, returns None.
    fn add_fot_entry(&self, handle: &ObjectHandle) -> Option<(*mut u8, usize)>;

    /// Map two objects in sequence, useful for executable loading. The default implementation makes
    /// no guarantees about ordering.
    fn map_two_objects(
        &self,
        in_id_a: ObjID,
        in_flags_a: MapFlags,
        in_id_b: ObjID,
        in_flags_b: MapFlags,
    ) -> Result<(ObjectHandle, ObjectHandle), MapError> {
        let map_and_check = |rev: bool| {
            let (id_a, flags_a) = if rev {
                (in_id_b, in_flags_b)
            } else {
                (in_id_a, in_flags_a)
            };

            let (id_b, flags_b) = if !rev {
                (in_id_b, in_flags_b)
            } else {
                (in_id_a, in_flags_a)
            };

            let a = self.map_object(id_a, flags_a)?;
            let b = self.map_object(id_b, flags_b)?;
            let a_addr = a.start as usize;
            let b_addr = b.start as usize;

            if rev && a_addr > b_addr {
                Ok((b, a))
            } else if !rev && b_addr > a_addr {
                Ok((a, b))
            } else {
                Err(MapError::InternalError)
            }
        };

        map_and_check(false).or_else(|_| map_and_check(true))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// Possible errors of join.
pub enum JoinError {
    /// The internal-thread-ID does not exist.
    LookupError,
    /// Join timed out.
    Timeout,
}

impl Display for JoinError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            JoinError::LookupError => write!(f, "lookup error"),
            JoinError::Timeout => write!(f, "operation timed out"),
        }
    }
}

impl core::error::Error for JoinError {}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
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

impl Display for MapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MapError::Other => write!(f, "unknown error"),
            MapError::InternalError => write!(f, "internal error"),
            MapError::OutOfResources => write!(f, "out of resources"),
            MapError::NoSuchObject => write!(f, "no such object"),
            MapError::PermissionDenied => write!(f, "permission denied"),
            MapError::InvalidArgument => write!(f, "invalid argument"),
        }
    }
}

impl core::error::Error for MapError {}

bitflags::bitflags! {
    /// Mapping protections for mapping objects into the address space.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct MapFlags: u32 {
        /// Read allowed.
        const READ = 1;
        /// Write allowed.
        const WRITE = 2;
        /// Exec allowed.
        const EXEC = 4;
    }
}

#[cfg_attr(feature = "kernel", allow(dead_code))]
/// A handle to an internal object. This has similar semantics to Arc, but since this crate
/// must be #[no_std], we need to implement refcounting ourselves.
pub struct ObjectHandle {
    /// Pointer to refcounter.
    pub internal_refs: NonNull<InternalHandleRefs>,
    /// The ID of the object.
    pub id: ObjID,
    /// The flags of this handle.
    pub flags: MapFlags,
    /// A pointer to the object's start (null-page, not base).
    pub start: *mut u8,
    /// A pointer to the object's metadata.
    pub meta: *mut u8,
}

impl core::fmt::Debug for ObjectHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ObjectHandle")
            .field("id", &self.id)
            .field("flags", &self.flags)
            .field("start", &self.start)
            .field("meta", &self.meta)
            .finish()
    }
}

unsafe impl Send for ObjectHandle {}
unsafe impl Sync for ObjectHandle {}

pub struct InternalHandleRefs {
    count: AtomicUsize,
}

impl Default for InternalHandleRefs {
    fn default() -> Self {
        Self {
            count: AtomicUsize::new(1),
        }
    }
}

impl ObjectHandle {
    pub fn new(
        internal_refs: NonNull<InternalHandleRefs>,
        id: ObjID,
        flags: MapFlags,
        start: *mut u8,
        meta: *mut u8,
    ) -> Self {
        Self {
            internal_refs,
            id,
            flags,
            start,
            meta,
        }
    }
}

impl Clone for ObjectHandle {
    fn clone(&self) -> Self {
        let rc = unsafe { self.internal_refs.as_ref() };
        // This use of Relaxed ordering is justified by https://doc.rust-lang.org/nomicon/arc-mutex/arc-clone.html.
        let old_count = rc.count.fetch_add(1, Ordering::Relaxed);
        // The above link also justifies the following behavior. If our count gets this high, we
        // have probably run into a problem somewhere.
        if old_count >= isize::MAX as usize {
            get_runtime().abort();
        }
        Self {
            internal_refs: self.internal_refs,
            id: self.id,
            flags: self.flags,
            start: self.start,
            meta: self.meta,
        }
    }
}

impl Drop for ObjectHandle {
    fn drop(&mut self) {
        // This use of Release ordering is justified by https://doc.rust-lang.org/nomicon/arc-mutex/arc-clone.html.
        let rc = unsafe { self.internal_refs.as_ref() };
        if rc.count.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        // This fence is needed to prevent reordering of the use and deletion
        // of the data.
        core::sync::atomic::fence(Ordering::Acquire);
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

    /// Exit the calling thread. This is allowed to cause a full exit of the entire program and all
    /// threads.
    fn exit(&self, code: i32) -> !;

    /// Thread abort. This is allowed to cause a full exit of the entire program and all threads.
    fn abort(&self) -> !;

    /// Called by rt0 code to start the runtime. Once the runtime has initialized, it should call
    /// the provided entry function. The pointer arg is a pointer to an array of [AuxEntry] that
    /// terminates with an [AuxEntry::Null].
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

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// Possible errors returned by the FsRuntime
pub enum FsError {
    /// Error is unclassified.
    Other,
    /// Path provided isn't a valid u128 integer
    InvalidPath,
    /// Couldn't find the file descriptor
    LookupError,
    /// Seek is beyond maximum file size or before 0
    SeekError,
}

impl Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FsError::Other => write!(f, "unknown error"),
            FsError::InvalidPath => write!(f, "Path is invalid"),
            FsError::LookupError => write!(f, "Couldn't find file descriptor"),
            FsError::SeekError => write!(f, "Couldn't seek to this position"),
        }
    }
}

impl core::error::Error for FsError {}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// Enum of the possible ways to seek within a object
pub enum SeekFrom {
    /// Sets to the offset in bytes
    Start(u64),
    /// Sets to the offset relative to the end of the file
    End(i64),
    /// Sets the offset relative to the position of the cursor
    Current(i64),
}

/// A identifier for a Twizzler object that allows File-like IO
/// The data backing RawFd holds the position of the file cursor and a reference to the object that
/// stores the file's data.
pub type RawFd = u32;

/// Runtime that implements STD's FS support. Currently being implemented.
pub trait RustFsRuntime {
    /// Takes in a u128 integer as CStr and emits a File Descriptor that allows File-Like IO on a
    /// Twizzler Object. Note that the object must already exist to be opened.
    fn open(&self, path: &CStr) -> Result<RawFd, FsError>;

    /// Reads bytes from the source twizzler Object into the specified buffer, returns how many
    /// bytes were read.
    fn read(&self, fd: RawFd, buf: &mut [u8]) -> Result<usize, FsError>;

    /// Writes bytes from the source twizzler Object into the specified buffer, returns how many
    /// bytes were written.
    fn write(&self, fd: RawFd, buf: &[u8]) -> Result<usize, FsError>;

    /// Cleans the data associated with the RawFd allowing reuse. Note that this doesn't
    /// close/unmap the backing object.
    fn close(&self, fd: RawFd) -> Result<(), FsError>;

    /// Moves the cursor to a specified offset within the backed object.
    fn seek(&self, fd: RawFd, pos: SeekFrom) -> Result<usize, FsError>;
}

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
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
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

impl Display for ReadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReadError::Other => write!(f, "unknown error"),
            ReadError::IoError => write!(f, "I/O error"),
            ReadError::PermissionDenied => write!(f, "permission denied"),
            ReadError::NoIo => write!(f, "no such I/O mechanism"),
        }
    }
}

impl core::error::Error for ReadError {}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
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

impl Display for WriteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WriteError::Other => write!(f, "unknown error"),
            WriteError::IoError => write!(f, "I/O error"),
            WriteError::PermissionDenied => write!(f, "permission denied"),
            WriteError::NoIo => write!(f, "no such I/O mechanism"),
        }
    }
}

impl core::error::Error for WriteError {}

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
    /// The ID of this library.
    pub id: LibraryId,
    /// How this library is mapped.
    pub mapping: ObjectHandle,
    /// Actual range of addresses that comprise the library binary data.
    pub range: AddrRange,
    /// Information for dl_iterate_phdr
    pub dl_info: Option<DlPhdrInfo>,
    /// The Library ID of first dependency.
    pub next_id: Option<LibraryId>,
}

impl AsRef<Library> for Library {
    fn as_ref(&self) -> &Library {
        self
    }
}

impl Library {
    pub fn name(&self) -> Option<&CStr> {
        unsafe { Some(CStr::from_ptr(self.dl_info?.name as *const i8)) }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Internal library ID type.
pub struct LibraryId(pub usize);

/// The runtime must ensure that the addresses are constant for the whole life of the library type,
/// and that all threads may see the type.
unsafe impl Send for Library {}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub type ElfAddr = usize;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub type ElfHalf = u32;

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct DlPhdrInfo {
    pub addr: ElfAddr,
    pub name: *const u8,
    pub phdr_start: *const u8,
    pub phdr_num: ElfHalf,
    pub _adds: core::ffi::c_longlong,
    pub _subs: core::ffi::c_longlong,
    pub modid: core::ffi::c_size_t,
    pub tls_data: *const core::ffi::c_void,
}

/// Functions for the debug support part of libstd (e.g. unwinding, backtracing).
pub trait DebugRuntime {
    /// Gets a handle to a library given the ID.
    fn get_library(&self, id: LibraryId) -> Option<Library>;
    /// Returns the ID of the main executable, if there is one.
    fn get_exeid(&self) -> Option<LibraryId>;
    /// Get a segment of a library, if the segment index exists. All segment IDs are indexes, so
    /// they range from [0, N).
    fn get_library_segment(&self, lib: &Library, seg: usize) -> Option<AddrRange>;
    /// Get the full mapping of the underlying library.
    fn get_full_mapping(&self, lib: &Library) -> Option<ObjectHandle>;
    /// Handler for calls to the dl_iterate_phdr call.
    fn iterate_phdr(&self, f: &mut dyn FnMut(DlPhdrInfo) -> core::ffi::c_int) -> core::ffi::c_int;
    /// Get the library ID immediately following the given one.
    fn next_library_id(&self, id: LibraryId) -> Option<LibraryId> {
        Some(LibraryId(id.0 + 1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
/// An address range.
pub struct AddrRange {
    /// Starting virtual address.
    pub start: usize,
    /// Length of the range.
    pub len: usize,
}

extern "rust-call" {
    /// Called by get_runtime to actually get the runtime.
    #[linkage = "extern_weak"]
    fn __twz_get_runtime(_a: ()) -> &'static (dyn Runtime + Sync);
}

/// Wrapper around call to __twz_get_runtime.
pub fn get_runtime() -> &'static (dyn Runtime + Sync) {
    unsafe { __twz_get_runtime(()) }
}

#[cfg(feature = "kernel")]
pub mod __imp {
    #[linkage = "weak"]
    #[no_mangle]
    pub unsafe extern "C" fn __twz_get_runtime() {
        core::intrinsics::abort();
    }
}

/// Public definition of __tls_get_addr, a function that gets automatically called by the compiler
/// when needed for TLS pointer resolution.
#[cfg(feature = "rustc-dep-of-std")]
#[no_mangle]
pub unsafe extern "C" fn __tls_get_addr(arg: usize) -> *const u8 {
    // Just call the runtime.
    let runtime = crate::get_runtime();
    let index = (arg as *const crate::TlsIndex)
        .as_ref()
        .expect("null pointer passed to __tls_get_addr");
    runtime
        .tls_get_addr(index)
        .expect("index passed to __tls_get_addr is invalid")
}

/// Public definition of dl_iterate_phdr, used by libunwind for learning where loaded objects
/// (executables, libraries, ...) are.
#[cfg(feature = "rustc-dep-of-std")]
#[no_mangle]
pub unsafe extern "C" fn dl_iterate_phdr(
    callback: extern "C" fn(
        ptr: *const DlPhdrInfo,
        sz: core::ffi::c_size_t,
        data: *mut core::ffi::c_void,
    ) -> core::ffi::c_int,
    data: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    let runtime = crate::get_runtime();
    runtime.iterate_phdr(&mut |info| callback(&info, core::mem::size_of::<DlPhdrInfo>(), data))
}
