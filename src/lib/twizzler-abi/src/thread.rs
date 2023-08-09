//! Functions for manipulating threads.

use core::{
    alloc::Layout,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(not(feature = "kernel"))]
use core::time::Duration;

use crate::marker::BaseType;

#[cfg(not(feature = "kernel"))]
use crate::syscall::*;

#[allow(unused_imports)]
use crate::{
    object::{ObjID, Protections},
    simple_idcounter::IdCounter,
    syscall::{MapFlags, ThreadSpawnArgs, ThreadSpawnFlags},
};

pub mod event;
/// Base type for a thread object.
#[derive(Default)]
#[repr(C)]
pub struct ThreadRepr {
    version: u32,
    flags: u32,
    #[cfg(not(feature = "kernel"))]
    status: AtomicU64,
    #[cfg(feature = "kernel")]
    pub status: AtomicU64,
    code: AtomicU64,
}

impl BaseType for ThreadRepr {
    fn init<T>(_t: T) -> Self {
        Self::default()
    }

    fn tags() -> &'static [(crate::marker::BaseVersion, crate::marker::BaseTag)] {
        todo!()
    }
}

/// Possible execution states for a thread. The transitions available are:
/// +------------+     +-----------+     +-------------+
/// |  Sleeping  +<--->+  Running  +<--->+  Suspended  |
/// +------------+     +-----+-----+     +-------------+
///                          |
///                          |   +----------+
///                          +-->+  Exited  |
///                              +----------+
/// The kernel will not transition a thread out of the exited state.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(u8)]
pub enum ExecutionState {
    /// The thread is running or waiting to be scheduled on a CPU.
    Running,
    /// The thread is sleeping, waiting for a condition in-kernel.
    Sleeping,
    /// The thread is suspended, and will not resume until manually transitioned back to running.
    Suspended,
    /// The thread has terminated, and will never run again.
    Exited = 255,
}

impl ExecutionState {
    fn from_status(status: u64) -> Self {
        // If we see a status we don't understand, just assume the thread is running.
        match status & 0xff {
            1 => ExecutionState::Sleeping,
            2 => ExecutionState::Suspended,
            255 => ExecutionState::Exited,
            _ => ExecutionState::Running,
        }
    }
}

impl ThreadRepr {
    pub fn get_state(&self) -> ExecutionState {
        let status = self.status.load(Ordering::Acquire);
        ExecutionState::from_status(status)
    }

    pub fn get_code(&self) -> u64 {
        self.code.load(Ordering::SeqCst)
    }

    pub fn set_state(&self, state: ExecutionState, code: u64) -> ExecutionState {
        let mut old_status = self.status.load(Ordering::SeqCst);
        loop {
            let old_state = ExecutionState::from_status(old_status);
            if old_state == ExecutionState::Exited {
                return old_state;
            }

            let status = state as u8 as u64;
            if state == ExecutionState::Exited {
                self.code.store(code, Ordering::SeqCst);
            }

            let result = self.status.compare_exchange(
                old_status,
                status,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            match result {
                Ok(_) => {
                    if !(old_state == ExecutionState::Running && state == ExecutionState::Sleeping
                        || old_state == ExecutionState::Sleeping
                            && state == ExecutionState::Running)
                        && old_state != state
                    {
                        #[cfg(not(feature = "kernel"))]
                        let _ = sys_thread_sync(
                            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                                ThreadSyncReference::Virtual(&self.status),
                                usize::MAX,
                            ))],
                            None,
                        );
                    }
                    return old_state;
                }
                Err(x) => {
                    old_status = x;
                }
            }
        }
    }

    #[cfg(not(feature = "kernel"))]
    /// Wait for a thread's status to change, optionally timing out. Return value is None if timeout occurs, or
    /// Some((ExecutionState, code)) otherwise.
    pub fn wait(&self, timeout: Option<Duration>) -> Option<(ExecutionState, u64)> {
        let mut status = self.get_state();
        loop {
            if status != ExecutionState::Running {
                return Some((status, self.code.load(Ordering::SeqCst)));
            }

            let op = ThreadSync::new_sleep(ThreadSyncSleep::new(
                crate::syscall::ThreadSyncReference::Virtual(&self.status),
                0,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ));
            sys_thread_sync(&mut [op], timeout).unwrap();
            status = self.get_state();
            if timeout.is_some() && status == ExecutionState::Running {
                return None;
            }
        }
    }
}

#[allow(dead_code)]
struct Thread {
    objid: ObjID,
    ptr: *mut ThreadRepr,
    slot: usize,
    tls_base: *const u8,
    tls_len: usize,
    tls_align: usize,
    stack_base: *const u8,
    stack_len: usize,
    internal_id: u32,
}

impl Thread {
    fn get_repr(&self) -> &ThreadRepr {
        unsafe { self.ptr.as_ref().unwrap() }
    }
}

static THREADS_LOCK: crate::simple_mutex::Mutex = crate::simple_mutex::Mutex::new();
static mut THREADS: *mut Thread = ptr::null_mut();
static mut THREADS_LEN: usize = 0;
static mut THREAD_IDS: IdCounter = IdCounter::new(1);

const STACK_ALIGN: usize = 32;

/// Build new thread internal tracking info.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
unsafe fn new_thread(
    objid: ObjID,
    base: *mut ThreadRepr,
    tls_base: *const u8,
    tls_len: usize,
    tls_align: usize,
    stack_base: *const u8,
    stack_len: usize,
    slot: usize,
) -> u32 {
    assert!(THREADS_LOCK.is_locked());
    let id = THREAD_IDS.next();

    if id as usize >= THREADS_LEN {
        let new_len = core::cmp::max(THREADS_LEN * 2, 16);
        let new_size = new_len * core::mem::size_of::<Thread>();
        let old_size = THREADS_LEN * core::mem::size_of::<Thread>();
        let layout = Layout::from_size_align(old_size, core::mem::align_of::<Thread>()).unwrap();
        THREADS = crate::alloc::global_realloc(THREADS as *mut u8, layout, new_size) as *mut Thread;
        THREADS_LEN = new_len;
    }

    let slice = core::slice::from_raw_parts_mut(THREADS, THREADS_LEN);
    slice[id as usize] = Thread {
        objid,
        ptr: base,
        internal_id: id,
        tls_base,
        tls_len,
        tls_align,
        stack_base,
        stack_len,
        slot,
    };

    id
}

unsafe fn release_thread(id: u32) {
    assert!(THREADS_LOCK.is_locked());
    let (stack_base, stack_len, tls_base, tls_len, tls_align) = {
        let slice = core::slice::from_raw_parts_mut(THREADS, THREADS_LEN);
        let info = (
            slice[id as usize].stack_base,
            slice[id as usize].stack_len,
            slice[id as usize].tls_base,
            slice[id as usize].tls_len,
            slice[id as usize].tls_align,
        );
        if slice[id as usize].ptr.is_null() {
            // already released
            return;
        }
        slice[id as usize].ptr = ptr::null_mut();
        slice[id as usize].objid = ObjID::new(0);
        slice[id as usize].internal_id = 0;
        THREAD_IDS.release(id);
        info
    };
    let tls_layout = Layout::from_size_align(tls_len, tls_align).unwrap();
    let stack_layout = Layout::from_size_align(stack_len, STACK_ALIGN).unwrap();
    crate::alloc::global_free(tls_base as *mut u8, tls_layout);
    crate::alloc::global_free(stack_base as *mut u8, stack_layout);
}

#[cfg(any(doc, feature = "rt"))]
/// Spawn a new thread, allocating a new stack for it, starting it at the specified entry point with
/// the argument `arg`. Returns the new internal ID of the thread, or None on failure.
/// # Safety
/// Caller must ensure that the thread doesn't run out of stack, and that entry pointer refers to a
/// valid address to start executing code.
pub unsafe fn spawn(stack_size: usize, entry: usize, arg: usize) -> Option<u32> {
    let stack_layout = Layout::from_size_align(stack_size, STACK_ALIGN).unwrap();
    let stack_base = crate::alloc::global_alloc(stack_layout);
    let (tls_set, tls_base, tls_len, tls_align) = crate::rt1::new_thread_tls().unwrap();
    let tls_layout = Layout::from_size_align(tls_len, tls_align).unwrap();
    let args = ThreadSpawnArgs::new(
        entry,
        stack_base as usize,
        stack_size,
        tls_set,
        arg,
        ThreadSpawnFlags::empty(),
        None,
    );
    let slot = crate::slot::global_allocate().or_else(|| {
        crate::alloc::global_free(stack_base, stack_layout);
        crate::alloc::global_free(tls_base, tls_layout);
        None
    })?;
    THREADS_LOCK.lock();
    let res = crate::syscall::sys_spawn(args);
    if let Ok(objid) = res {
        let mapres = crate::syscall::sys_object_map(
            None,
            objid,
            slot,
            Protections::READ | Protections::WRITE,
            MapFlags::empty(),
        );
        if mapres.is_ok() {
            let (base, _) = crate::slot::to_vaddr_range(slot);
            let internal_id = new_thread(
                objid,
                base as *mut ThreadRepr,
                tls_base,
                tls_len,
                tls_align,
                stack_base,
                stack_size,
                slot,
            );
            THREADS_LOCK.unlock();
            return Some(internal_id);
        }
    }
    THREADS_LOCK.unlock();
    crate::alloc::global_free(stack_base, stack_layout);
    crate::alloc::global_free(tls_base, tls_layout);
    crate::slot::global_release(slot);
    None
}

/// Wait until the specified thread terminates.
/// # Safety
/// The thread ID must be a valid thread ID.
pub unsafe fn join(id: u32) {
    THREADS_LOCK.lock();
    loop {
        let slice = core::slice::from_raw_parts(THREADS, THREADS_LEN);
        let repr = slice[id as usize].get_repr();
        if repr.status.load(Ordering::SeqCst) == 0 {
            let ts = crate::syscall::ThreadSync::new_sleep(crate::syscall::ThreadSyncSleep::new(
                crate::syscall::ThreadSyncReference::Virtual(&repr.status),
                0,
                crate::syscall::ThreadSyncOp::Equal,
                crate::syscall::ThreadSyncFlags::empty(),
            ));
            THREADS_LOCK.unlock();
            let _ = crate::syscall::sys_thread_sync(&mut [ts], None);
            THREADS_LOCK.lock();
        } else {
            break;
        }
    }
    release_thread(id);
    THREADS_LOCK.unlock();
}

/// Exit the current thread.
pub fn exit() -> ! {
    crate::syscall::sys_thread_exit(0);
}
