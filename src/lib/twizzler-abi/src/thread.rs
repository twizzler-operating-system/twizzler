//! Functions for manipulating threads.

use core::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(feature = "kernel"))]
use core::time::Duration;

#[cfg(not(feature = "kernel"))]
use crate::syscall::*;
use crate::syscall::{ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep};
#[allow(unused_imports)]
use crate::{
    object::{ObjID, Protections},
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

    /// Create a [ThreadSyncSleep] that will wait until the thread's state matches `state`.
    pub fn waitable(&self, state: ExecutionState) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.status),
            state as u64,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::INVERT,
        )
    }

    /// Create a [ThreadSyncSleep] that will wait until the thread's state is _not_ `state`.
    pub fn waitable_until_not(&self, state: ExecutionState) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.status),
            state as u64,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    #[cfg(not(feature = "kernel"))]
    /// Wait for a thread's status to change, optionally timing out. Return value is None if timeout
    /// occurs, or Some((ExecutionState, code)) otherwise.
    pub fn wait(
        &self,
        expected: ExecutionState,
        timeout: Option<Duration>,
    ) -> Option<(ExecutionState, u64)> {
        let mut status = self.get_state();
        loop {
            if status != expected {
                return Some((status, self.code.load(Ordering::SeqCst)));
            }
            let op = self.waitable_until_not(expected);
            sys_thread_sync(&mut [ThreadSync::new_sleep(op)], timeout).unwrap();
            status = self.get_state();
            if timeout.is_some() && status == expected {
                return None;
            }
        }
    }

    #[cfg(not(feature = "kernel"))]
    /// Wait for a thread's status reach a target value, or exited, optionally timing out. The
    /// actual execution state of the thread is returned.
    pub fn wait_until(
        &self,
        target: ExecutionState,
        timeout: Option<Duration>,
    ) -> Option<(ExecutionState, u64)> {
        let mut status = self.get_state();
        loop {
            if status == target {
                return Some((status, self.code.load(Ordering::SeqCst)));
            }
            let op = self.waitable(target);
            sys_thread_sync(&mut [ThreadSync::new_sleep(op)], timeout).unwrap();
            status = self.get_state();
            if timeout.is_some() && status != target {
                return None;
            }
        }
    }
}
