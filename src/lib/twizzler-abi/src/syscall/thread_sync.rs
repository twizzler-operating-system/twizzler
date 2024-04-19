use core::{
    ptr,
    sync::atomic::{AtomicU32, AtomicU64},
    time::Duration,
};

use bitflags::bitflags;
use num_enum::{FromPrimitive, IntoPrimitive};

use crate::{arch::syscall::raw_syscall, object::ObjID};

use super::{convert_codes_to_result, Syscall};
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
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    Virtual32(*const AtomicU32),
}
unsafe impl Send for ThreadSyncReference {}

impl ThreadSyncReference {
    pub fn load(&self) -> u64 {
        match self {
            ThreadSyncReference::ObjectRef(_, _) => todo!(),
            ThreadSyncReference::Virtual(p) => {
                unsafe { &**p }.load(core::sync::atomic::Ordering::SeqCst)
            }
            ThreadSyncReference::Virtual32(p) => unsafe { &**p }
                .load(core::sync::atomic::Ordering::SeqCst)
                .into(),
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

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    Hash,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error returns for [sys_thread_sync].
pub enum ThreadSyncError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
    /// Invalid reference.
    #[error("invalid reference")]
    InvalidReference = 2,
    /// The operation timed out.
    #[error("operation timed out")]
    Timeout = 3,
}

impl core::error::Error for ThreadSyncError {}

/// Result of sync operations.
pub type ThreadSyncResult = Result<usize, ThreadSyncError>;

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
