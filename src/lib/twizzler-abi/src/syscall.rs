//! Wrapper functions around for raw_syscall, providing a typed and safer way to interact with the kernel.

use bitflags::bitflags;
use core::{fmt, num::NonZeroUsize, ptr, sync::atomic::AtomicU64, time::Duration};

use crate::{
    arch::syscall::raw_syscall,
    object::{ObjID, Protections},
};
#[derive(Copy, Clone, Debug)]
#[repr(C)]
/// All possible Synchronous syscalls into the Twizzler kernel.
pub enum Syscall {
    Null = 0,
    /// Read data from the kernel console, either buffer or input.
    KernelConsoleRead = 1,
    /// Write data to the kernel console.
    KernelConsoleWrite = 2,
    /// Sync a thread with other threads using some number of memory words.
    ThreadSync = 3,
    /// General thread control functions.
    ThreadCtrl = 4,
    /// Create new object.
    ObjectCreate = 5,
    /// Map an object into address space.
    ObjectMap = 6,
    /// Returns system info
    SysInfo = 7,
    /// Spawn a new thread.
    Spawn = 8,
    MaxSyscalls = 9,
}

impl Syscall {
    /// Return the number associated with this syscall.
    pub fn num(&self) -> u64 {
        *self as u64
    }
}

impl From<usize> for Syscall {
    fn from(x: usize) -> Self {
        if x >= Syscall::MaxSyscalls as usize {
            return Syscall::Null;
        }
        unsafe { core::intrinsics::transmute(x as u32) }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// Possible errors returned by reading from the kernel console's input.
pub enum KernelConsoleReadError {
    /// Operation would block, but non-blocking was requested.
    WouldBlock = 0,
    /// Failed to read because there was no input mechanism made available to the kernel.
    NoSuchDevice = 1,
    /// The input mechanism had an internal error.
    IOError = 2,
}

impl KernelConsoleReadError {
    fn as_str(&self) -> &str {
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
    /// Flags to pass to [sys_kernel_console_read].
    pub struct KernelConsoleReadFlags: u64 {
        /// If the read would block, return instead.
        const NONBLOCKING = 1;
    }
}

/// Read from the kernel console input, placing data into `buffer`.
///
/// This is the INPUT mechanism, and not the BUFFER mechanism. For example, if the kernel console is
/// a serial port, the input mechanism is the reading side of the serial console. To read from the
/// kernel console output buffer, use [sys_kernel_console_read_buffer].
///
/// Returns the number of bytes read on success and [KernelConsoleReadError] on failure.
pub fn sys_kernel_console_read(
    _buffer: &mut [u8],
    _flags: KernelConsoleReadFlags,
) -> Result<usize, KernelConsoleReadError> {
    todo!()
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// Possible errors returned by reading from the kernel console's buffer.
pub enum KernelConsoleReadBufferError {
    /// Operation would block, but non-blocking was requested.
    WouldBlock = 0,
}

impl KernelConsoleReadBufferError {
    fn as_str(&self) -> &str {
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
    /// Flags to pass to [sys_kernel_console_read_buffer].
    pub struct KernelConsoleReadBufferFlags: u64 {
        /// If the operation would block, return instead.
        const NONBLOCKING = 1;
    }
}

/// Read from the kernel console buffer, placing data into `buffer`.
///
/// This is the BUFFER mechanism, and not the INPUT mechanism. All writes to the kernel console get
/// placed in the buffer and copied out to the underlying console device in the kernel. If you want
/// to read from the INPUT device, see [sys_kernel_console_read].
///
/// Returns the number of bytes read on success and [KernelConsoleReadBufferError] on failure.
pub fn sys_kernel_console_read_buffer(
    _buffer: &mut [u8],
    _flags: KernelConsoleReadBufferFlags,
) -> Result<usize, KernelConsoleReadBufferError> {
    todo!()
}

bitflags! {
    /// Flags to pass to [sys_kernel_console_write].
    pub struct KernelConsoleWriteFlags: u64 {
        /// If the buffer is full, discard this write instead of overwriting old data.
        const DISCARD_ON_FULL = 1;
    }
}

/// Write to the kernel console.
///
/// This writes first to the kernel console buffer, for later reading by
/// [sys_kernel_console_read_buffer], and then writes to the underlying kernel console device (if
/// one is present). By default, if the buffer is full, this write will overwrite old data in the
/// (circular) buffer, but this behavior can be controlled by the `flags` argument.
///
/// This function cannot fail.
pub fn sys_kernel_console_write(buffer: &[u8], flags: KernelConsoleWriteFlags) {
    let arg0 = buffer.as_ptr() as usize as u64;
    let arg1 = buffer.len() as u64;
    let arg2 = flags.bits();
    unsafe {
        raw_syscall(Syscall::KernelConsoleWrite, &[arg0, arg1, arg2]);
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u64)]
/// Possible Thread Control operations
pub enum ThreadControl {
    /// Exit the thread. arg1 and arg2 should be code and location respectively, where code contains
    /// a 64-bit value to write into *location, followed by the kernel performing a thread-wake
    /// event on the memory word at location. If location is null, the write and thread-wake do not occur.
    Exit = 0,
    /// Yield the thread's CPU time now. The actual effect of this is unspecified, but it acts as a
    /// hint to the kernel that this thread does not need to run right now. The kernel, of course,
    /// is free to ignore this hint.
    Yield = 1,
    /// Set thread's TLS pointer
    SetTls = 2,
}

impl From<u64> for ThreadControl {
    fn from(x: u64) -> Self {
        match x {
            0 => Self::Exit,
            1 => Self::Yield,
            2 => Self::SetTls,
            _ => Self::Yield,
        }
    }
}

/// Exit the thread. arg1 and arg2 should be code and location respectively, where code contains
/// a 64-bit value to write into *location, followed by the kernel performing a thread-wake
/// event on the memory word at location. If location is null, the write and thread-wake do not occur.
pub fn sys_thread_exit(code: u64, location: *mut u64) -> ! {
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[ThreadControl::Exit as u64, code, location as u64],
        );
    }
    unreachable!()
}

/// Yield the thread's CPU time now. The actual effect of this is unspecified, but it acts as a
/// hint to the kernel that this thread does not need to run right now. The kernel, of course,
/// is free to ignore this hint.
pub fn sys_thread_yield() {
    unsafe {
        raw_syscall(Syscall::ThreadCtrl, &[ThreadControl::Yield as u64]);
    }
}

/// Set the current kernel thread's TLS pointer. On x86_64, for example, this changes user's FS
/// segment base to the supplies TLS value.
pub fn sys_thread_settls(tls: u64) {
    unsafe {
        raw_syscall(Syscall::ThreadCtrl, &[ThreadControl::SetTls as u64, tls]);
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible operations the kernel can perform when looking at the supplies reference and the
/// supplied value. If the operation `*reference OP value` evaluates to true (or false if the INVERT
/// flag is passed), then the thread is put
/// to sleep.
pub enum ThreadSyncOp {
    /// Compare for equality
    Equal = 0,
}

bitflags! {
    /// Flags to pass to sys_thread_sync.
    pub struct ThreadSyncFlags: u32 {
        /// Invert the decision test for sleeping the thread.
        const INVERT = 1;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// A reference to a piece of data. May either be a non-realized persistent reference or a virtual address.
pub enum ThreadSyncReference {
    ObjectRef(ObjID, u64),
    Virtual(*const AtomicU64),
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Specification for a thread sleep request.
pub struct ThreadSyncSleep {
    reference: ThreadSyncReference,
    value: u64,
    op: ThreadSyncOp,
    flags: ThreadSyncFlags,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Specification for a thread wake request.
pub struct ThreadSyncWake {
    reference: ThreadSyncReference,
    count: usize,
}

impl ThreadSyncSleep {
    /// Construct a new thread sync sleep request. The kernel will compare the 64-bit data at
    /// `*reference` with the passed `value` using `op` (and optionally inverting the result). If
    /// true, the kernel will put the thread to sleep until another thread comes along and performs
    /// a wake request on that same word of memory.
    ///
    /// References always refer to a particular 64-bit value inside of an object. If a virtual
    /// address is passed as a reference, it is first converted to an object-offset pair based on
    /// the current object mapping of the address space.
    pub fn new(
        reference: ThreadSyncReference,
        value: u64,
        op: ThreadSyncOp,
        flags: ThreadSyncFlags,
    ) -> Self {
        Self {
            reference,
            value,
            op,
            flags,
        }
    }
}

impl ThreadSyncWake {
    /// Construct a new thread wake request. The reference works the same was as in
    /// [ThreadSyncSleep]. The kernel will wake up `count` threads that are sleeping on this
    /// particular word of object memory. If you want to wake up all threads, you can supply `usize::MAX`.
    pub fn new(reference: ThreadSyncReference, count: usize) -> Self {
        Self { reference, count }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u64)]
/// Possible error returns for [sys_thread_sync].
pub enum ThreadSyncError {
    /// An unknown error.
    Unknown = 0,
    /// The reference was invalid.
    InvalidReference = 1,
    /// An argument was invalid.
    InvalidArgument = 2,
}

pub type ThreadSyncResult = Result<u64, ThreadSyncError>;

impl From<ThreadSyncError> for u64 {
    fn from(x: ThreadSyncError) -> Self {
        x as Self
    }
}

impl ThreadSyncError {
    /// Convert error to a human-readable string.
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "an argument was invalid",
            Self::InvalidReference => "a reference was invalid",
        }
    }
}

impl From<u64> for ThreadSyncError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidReference,
            2 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ThreadSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ThreadSyncError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Either a sleep or wake request. The syscall comprises of a number of either sleep or wake requests.
pub enum ThreadSync {
    Sleep(ThreadSyncSleep, ThreadSyncResult),
    Wake(ThreadSyncWake, ThreadSyncResult),
}

impl ThreadSync {
    /// Build a sleep request.
    pub fn new_sleep(sleep: ThreadSyncSleep) -> Self {
        Self::Sleep(sleep, Ok(0))
    }

    /// Build a wake request.
    pub fn new_wake(wake: ThreadSyncWake) -> Self {
        Self::Wake(wake, Ok(0))
    }
}

#[inline]
fn convert_codes_to_result<T, E, D, F, G>(code: u64, val: u64, d: D, f: F, g: G) -> Result<T, E>
where
    F: Fn(u64, u64) -> T,
    G: Fn(u64, u64) -> E,
    D: Fn(u64, u64) -> bool,
{
    if d(code, val) {
        Err(g(code, val))
    } else {
        Ok(f(code, val))
    }
}

#[inline]
fn justval<T: From<u64>>(_: u64, v: u64) -> T {
    v.into()
}

/// Perform a number of [ThreadSync] operations, either waking of threads waiting on words of
/// memory, or sleeping this thread on one or more words of memory (or both). The order these
/// requests are processed in is undefined.
///
/// The caller may optionally specify a timeout, causing this thread to sleep for at-most that
/// Duration. However, the exact time is not guaranteed (it may be less if the thread is woken up,
/// or slightly more due to scheduling uncertainty). If no operations are specified, the thread will
/// sleep until the timeout expires.
///
/// Returns either Ok(bool), indicating if the timeout expired or not, or Err([ThreadSyncError]),
/// indicating failure. After return, the kernel may have modified the ThreadSync entries to
/// indicate additional information about each request (errors or status).
pub fn sys_thread_sync(
    operations: &mut [ThreadSync],
    timeout: Option<Duration>,
) -> Result<bool, ThreadSyncError> {
    let ptr = operations.as_mut_ptr();
    let count = operations.len();
    let timeout = timeout
        .as_ref()
        .map_or(ptr::null(), |t| t as *const Duration);

    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadSync,
            &[ptr as u64, count as u64, timeout as u64],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        |_, v| v > 0,
        |_, v| ThreadSyncError::from(v),
    )
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Specifications for an object-copy from a source object. The specified ranges are
/// source:[src_start, src_start + len) copied to <some unspecified destination object>:[dest_start,
/// dest_start + len). Each range must start within an object, and end within the object.
pub struct ObjectSource {
    /// The ID of the source object.
    pub id: ObjID,
    /// The offset into the source object to start the copy.
    pub src_start: u64,
    /// The offset into the dest object to start the copy to.
    pub dest_start: u64,
    /// The length of the copy.
    pub len: usize,
}

impl ObjectSource {
    /// Construct a new ObjectSource.
    pub fn new(id: ObjID, src_start: u64, dest_start: u64, len: usize) -> Self {
        Self {
            id,
            src_start,
            dest_start,
            len,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// The backing memory type for this object. Currently doesn't do anything.
pub enum BackingType {
    /// The default, let the kernel decide based on the [LifetimeType] of the object.
    Normal = 0,
}

impl Default for BackingType {
    fn default() -> Self {
        Self::Normal
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// The base lifetime type of the object. Note that this does not ensure that the object is stored
/// in a specific type of memory, the kernel is allowed to migrate objects with the Normal
/// [BackingType] as it sees fit. For more information on object lifetime, see [the book](https://twizzler-operating-system.github.io/nightly/book/object_lifetime.html).
pub enum LifetimeType {
    /// This object is volatile, and is expected to be deleted after a power cycle.
    Volatile = 0,
    /// This object is persistent, and should be deleted only after an explicit delete call.
    Persistent = 1,
}

bitflags! {
    /// Flags to pass to the object create system call.
    pub struct ObjectCreateFlags: u32 {
    }
}

bitflags! {
    /// Flags controlling how a particular object tie operates.
    pub struct CreateTieFlags: u32 {
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Full object creation specification, minus ties.
pub struct ObjectCreate {
    kuid: ObjID,
    bt: BackingType,
    lt: LifetimeType,
    flags: ObjectCreateFlags,
}
impl ObjectCreate {
    /// Build a new object create specification.
    pub fn new(
        bt: BackingType,
        lt: LifetimeType,
        kuid: Option<ObjID>,
        flags: ObjectCreateFlags,
    ) -> Self {
        Self {
            kuid: kuid.unwrap_or_else(|| 0.into()),
            bt,
            lt,
            flags,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// A specification of ties to create.
/// (see [the book](https://twizzler-operating-system.github.io/nightly/book/object_lifetime.html) for more information on ties).
pub struct CreateTieSpec {
    id: ObjID,
    flags: CreateTieFlags,
}

impl CreateTieSpec {
    /// Create a new CreateTieSpec.
    pub fn new(id: ObjID, flags: CreateTieFlags) -> Self {
        Self { id, flags }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error returns for [sys_object_create].
pub enum ObjectCreateError {
    /// An unknown error occurred.
    Unknown = 0,
    /// One of the arguments was invalid.
    InvalidArgument = 1,
    /// A source or tie object was not found.
    ObjectNotFound = 2,
    /// The kernel could not handle one of the source ranges.
    SourceMisalignment = 3,
}

impl ObjectCreateError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "an argument was invalid",
            Self::ObjectNotFound => "a referenced object was not found",
            Self::SourceMisalignment => "a source specification had an unsatisfiable range",
        }
    }
}

impl From<ObjectCreateError> for u64 {
    fn from(x: ObjectCreateError) -> Self {
        x as Self
    }
}

impl From<u64> for ObjectCreateError {
    fn from(x: u64) -> Self {
        match x {
            3 => Self::SourceMisalignment,
            2 => Self::ObjectNotFound,
            1 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ObjectCreateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ObjectCreateError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

/// Create an object, returning either its ID or an error.
pub fn sys_object_create(
    create: ObjectCreate,
    sources: &[ObjectSource],
    ties: &[CreateTieSpec],
) -> Result<ObjID, ObjectCreateError> {
    let args = [
        &create as *const ObjectCreate as u64,
        sources.as_ptr() as u64,
        sources.len() as u64,
        ties.as_ptr() as u64,
        ties.len() as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectCreate, &args) };
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        crate::object::ObjID::new_from_parts,
        |_, v| ObjectCreateError::from(v),
    )
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_object_map].
pub enum ObjectMapError {
    /// An unknown error occurred.
    Unknown = 0,
    /// The specified object was not found.
    ObjectNotFound = 1,
    /// The specified slot was invalid.
    InvalidSlot = 2,
    /// The specified protections were invalid.
    InvalidProtections = 3,
}

impl ObjectMapError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidProtections => "invalid protections",
            Self::InvalidSlot => "invalid slot",
            Self::ObjectNotFound => "object was not found",
        }
    }
}

impl From<ObjectMapError> for u64 {
    fn from(x: ObjectMapError) -> u64 {
        x as u64
    }
}

impl From<u64> for ObjectMapError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::ObjectNotFound,
            2 => Self::InvalidSlot,
            3 => Self::InvalidProtections,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ObjectMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ObjectMapError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

bitflags! {
    /// Flags to pass to [sys_object_map].
    pub struct MapFlags: u32 {
    }
}

/// Map an object into the address space with the specified protections.
pub fn sys_object_map(
    id: ObjID,
    slot: usize,
    prot: Protections,
    flags: MapFlags,
) -> Result<usize, ObjectMapError> {
    let (hi, lo) = id.split();
    let args = [hi, lo, slot as u64, prot.bits() as u64, flags.bits() as u64];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectMap, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, justval)
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Information about the system.
pub struct SysInfo {
    /// The version of this data structure, to allow expansion.
    pub version: u32,
    /// Flags. Currently unused.
    pub flags: u32,
    /// The number of CPUs on this system. Hyperthreads are counted as individual CPUs.
    pub cpu_count: usize,
    /// The size of a virtual address page on this system.
    pub page_size: usize,
}

impl SysInfo {
    /// Get the number of CPUs on the system.
    pub fn cpu_count(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.cpu_count).expect("CPU count from sysinfo should always be non-zero")
    }

    /// Get the page size of the system.
    pub fn page_size(&self) -> usize {
        self.page_size
    }
}

/// Get a SysInfo struct from the kernel.
pub fn sys_info() -> SysInfo {
    let mut sysinfo = core::mem::MaybeUninit::<SysInfo>::zeroed();
    unsafe {
        raw_syscall(
            Syscall::SysInfo,
            &[&mut sysinfo as *mut core::mem::MaybeUninit<SysInfo> as u64],
        );
        sysinfo.assume_init()
    }
}

bitflags! {
    /// Flags to pass to [sys_spawn].
    pub struct ThreadSpawnFlags: u32 {
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Arguments to pass to [sys_spawn].
pub struct ThreadSpawnArgs {
    entry: *const u8,
    stack_base: *const u8,
    stack_size: usize,
    tls: *const u8,
    arg: usize,
    flags: ThreadSpawnFlags,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_spawn].
pub enum ThreadSpawnError {
    /// An unknown error occurred.
    Unknown = 0,
    /// One of the arguments was invalid.   
    InvalidArgument = 1,
}

impl ThreadSpawnError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidArgument => "invalid argument",
        }
    }
}

impl From<ThreadSpawnError> for u64 {
    fn from(x: ThreadSpawnError) -> Self {
        x as u64
    }
}
/*
impl Into<u64> for ThreadSpawnError {
    fn into(self) -> u64 {
        self as u64
    }
}
*/

impl From<u64> for ThreadSpawnError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ThreadSpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ThreadSpawnError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

/// Spawn a new thread, returning the ObjID of the thread's handle or an error.
/// # Safety
/// The caller must ensure that the [ThreadSpawnArgs] has sane values.
pub unsafe fn sys_spawn(args: ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    let (code, val) = raw_syscall(Syscall::Spawn, &[&args as *const ThreadSpawnArgs as u64]);
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        crate::object::ObjID::new_from_parts,
        |_, v| ThreadSpawnError::from(v),
    )
}
