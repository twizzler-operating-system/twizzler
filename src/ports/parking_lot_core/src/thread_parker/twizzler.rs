// Copyright 2016 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use core::sync::atomic::{AtomicU64, Ordering};
use std::{
    thread,
    time::{Duration, Instant},
};

use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};

// Helper type for putting a thread to sleep until some other thread wakes it up
pub struct ThreadParker {
    futex: AtomicU64,
}

impl super::ThreadParkerT for ThreadParker {
    type UnparkHandle = UnparkHandle;

    const IS_CHEAP_TO_CONSTRUCT: bool = true;

    #[inline]
    fn new() -> ThreadParker {
        ThreadParker {
            futex: AtomicU64::new(0),
        }
    }

    #[inline]
    unsafe fn prepare_park(&self) {
        self.futex.store(1, Ordering::Relaxed);
    }

    #[inline]
    unsafe fn timed_out(&self) -> bool {
        self.futex.load(Ordering::Relaxed) != 0
    }

    #[inline]
    unsafe fn park(&self) {
        while self.futex.load(Ordering::Acquire) != 0 {
            self.futex_wait(None);
        }
    }

    #[inline]
    unsafe fn park_until(&self, timeout: Instant) -> bool {
        while self.futex.load(Ordering::Acquire) != 0 {
            let now = Instant::now();
            if timeout <= now {
                return false;
            }
            let diff = timeout - now;
            self.futex_wait(Some(diff));
        }
        true
    }

    // Locks the parker to prevent the target thread from exiting. This is
    // necessary to ensure that thread-local ThreadData objects remain valid.
    // This should be called while holding the queue lock.
    #[inline]
    unsafe fn unpark_lock(&self) -> UnparkHandle {
        // We don't need to lock anything, just clear the state
        self.futex.store(0, Ordering::Release);

        UnparkHandle { futex: &self.futex }
    }
}

impl ThreadParker {
    /// The result is deliberately discarded, and the failure mode is worth knowing: `park()`
    /// loops until the futex word changes, so a *persistent* error return here becomes a tight
    /// spin -- inside the monitor's locks, with no diagnostic. Not reachable on any observed
    /// path, and left as-is on purpose: enumerating which `sys_thread_sync` errors are benign
    /// would mean shipping a new abort predicate into a lock primitive to guard against a
    /// failure never seen. If a stuck monitor ever traces here, this is what it is.
    #[inline]
    fn futex_wait(&self, ts: Option<Duration>) {
        let op = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.futex as *const AtomicU64),
            1,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], ts);
    }
}

pub struct UnparkHandle {
    futex: *const AtomicU64,
}

impl super::UnparkHandleT for UnparkHandle {
    #[inline]
    unsafe fn unpark(self) {
        // The thread data may have been freed at this point, but it doesn't
        // matter since the syscall will just fail in that case.
        let op = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(self.futex),
            usize::MAX,
        ));
        let _ = twizzler_abi::syscall::sys_thread_sync(&mut [op], None);
    }
}

#[inline]
pub fn thread_yield() {
    thread::yield_now();
}
