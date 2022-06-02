use core::{sync::atomic::AtomicU64, fmt, time::Duration, ptr};

use bitflags::bitflags;

use crate::{object::ObjID, arch::syscall::raw_syscall};

use super::{Syscall, convert_codes_to_result};
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

impl ThreadSyncOp {
    /// Apply the operation to two values, returning the result.
    pub fn check<T: Eq + PartialEq + Ord + PartialOrd>(&self, a: T, b: T) -> bool {
        match self {
            Self::Equal => a == b,
        }
    }
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
    ObjectRef(ObjID, usize),
    Virtual(*const AtomicU64),
}
unsafe impl Send for ThreadSyncReference {}

impl ThreadSyncReference {
    pub fn load(&self) -> u64 {
        match self {
            ThreadSyncReference::ObjectRef(_, _) => todo!(),
            ThreadSyncReference::Virtual(p) => {
                unsafe { &**p }.load(core::sync::atomic::Ordering::SeqCst)
            }
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Specification for a thread sleep request.
pub struct ThreadSyncSleep {
    /// Reference to an atomic u64 that we will compare to.
    pub reference: ThreadSyncReference,
    /// The value used for the comparison.
    pub value: u64,
    /// The operation to compare *reference and value to.
    pub op: ThreadSyncOp,
    /// Flags to apply to this sleep request.
    pub flags: ThreadSyncFlags,
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
/// Specification for a thread wake request.
pub struct ThreadSyncWake {
    /// Reference to the word for which we will wake up threads that have gone to sleep.
    pub reference: ThreadSyncReference,
    /// Number of threads to wake up.
    pub count: usize,
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

    pub fn ready(&self) -> bool {
        let st = self.reference.load();
        match self.op {
            ThreadSyncOp::Equal => st != self.value,
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
    /// The operation timed out.
    Timeout = 3,
}

pub type ThreadSyncResult = Result<usize, ThreadSyncError>;

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
            Self::Timeout => "the operation timed out",
        }
    }
}

impl From<u64> for ThreadSyncError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidReference,
            2 => Self::InvalidArgument,
            3 => Self::Timeout,
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

    /// Get the result of the thread sync operation.
    pub fn get_result(&self) -> ThreadSyncResult {
        match self {
            ThreadSync::Sleep(_, e) => *e,
            ThreadSync::Wake(_, e) => *e,
        }
    }

    pub fn ready(&self) -> bool {
        match self {
            ThreadSync::Sleep(o, _) => o.ready(),
            ThreadSync::Wake(_, _) => true,
        }
    }
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
/// Returns either Ok(ready_count), indicating how many operations were immediately ready, or Err([ThreadSyncError]),
/// indicating failure. After return, the kernel may have modified the ThreadSync entries to
/// indicate additional information about each request, with Err to indicate error and Ok(n) to
/// indicate success. For sleep requests, n is 0 if the operation went to sleep or 1 otherwise. For
/// wakeup requests, n indicates the number of threads woken up by this operation.
///
/// Note that spurious wakeups are possible, and that even if a timeout occurs the function may
/// return Ok(0).
pub fn sys_thread_sync(
    operations: &mut [ThreadSync],
    timeout: Option<Duration>,
) -> Result<usize, ThreadSyncError> {
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
        |c, _| c != 0,
        |_, v| v as usize,
        |_, v| ThreadSyncError::from(v),
    )
}