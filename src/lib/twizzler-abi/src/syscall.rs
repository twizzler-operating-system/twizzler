use bitflags::bitflags;
use core::{fmt, ptr, sync::atomic::AtomicU64, time::Duration};

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
    MaxSyscalls = 7,
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

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
pub enum ThreadSyncOp {
    Equal,
}

bitflags! {
    pub struct ThreadSyncFlags: u32 {
        const INVERT = 1;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum ThreadSyncReference {
    ObjectRef(ObjID, u64),
    Virtual(*const AtomicU64),
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct ThreadSyncSleep {
    reference: ThreadSyncReference,
    value: u64,
    op: ThreadSyncOp,
    flags: ThreadSyncFlags,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct ThreadSyncWake {
    reference: ThreadSyncReference,
    count: usize,
}

impl ThreadSyncSleep {
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
    pub fn new(reference: ThreadSyncReference, count: usize) -> Self {
        Self { reference, count }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u64)]
pub enum ThreadSyncError {
    Unknown = 0,
    InvalidReference = 1,
    InvalidArgument = 2,
}

pub type ThreadSyncResult = Result<u64, ThreadSyncError>;

impl Into<u64> for ThreadSyncError {
    fn into(self) -> u64 {
        self as u64
    }
}

impl ThreadSyncError {
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
pub enum ThreadSync {
    Sleep(ThreadSyncSleep, ThreadSyncResult),
    Wake(ThreadSyncWake, ThreadSyncResult),
}

impl ThreadSync {
    pub fn new_sleep(sleep: ThreadSyncSleep) -> Self {
        Self::Sleep(sleep, Ok(0))
    }
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
pub struct ObjectSource {
    pub id: ObjID,
    pub src_start: u64,
    pub dest_start: u64,
    pub len: usize,
}

impl ObjectSource {
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
pub enum BackingType {
    Normal = 0,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub enum LifetimeType {
    Volatile = 0,
    Persistent = 1,
}

bitflags! {
    pub struct ObjectCreateFlags: u32 {
    }
}

bitflags! {
    pub struct CreateTieFlags: u32 {
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct ObjectCreate {
    kuid: ObjID,
    bt: BackingType,
    lt: LifetimeType,
    flags: ObjectCreateFlags,
}

impl ObjectCreate {
    pub fn new(
        bt: BackingType,
        lt: LifetimeType,
        kuid: Option<ObjID>,
        flags: ObjectCreateFlags,
    ) -> Self {
        Self {
            kuid: kuid.unwrap_or(0.into()),
            bt,
            lt,
            flags,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct CreateTieSpec {
    id: ObjID,
    flags: CreateTieFlags,
}

impl CreateTieSpec {
    pub fn new(id: ObjID, flags: CreateTieFlags) -> Self {
        Self { id, flags }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
pub enum ObjectCreateError {
    Unknown = 0,
    InvalidArgument = 1,
    ObjectNotFound = 2,
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

impl Into<u64> for ObjectCreateError {
    fn into(self) -> u64 {
        self as u32 as u64
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
        |c, v| crate::object::objid_from_parts(c, v),
        |_, v| ObjectCreateError::from(v),
    )
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
pub enum ObjectMapError {
    Unknown = 0,
    ObjectNotFound = 1,
    InvalidSlot = 2,
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

impl Into<u64> for ObjectMapError {
    fn into(self) -> u64 {
        self as u64
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
    pub struct MapFlags: u32 {
    }
}

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
