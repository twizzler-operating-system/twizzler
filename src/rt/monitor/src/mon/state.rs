//! Global monitor state: a flags word every compartment can read, or bits into, and block on.
//!
//! A plain static rather than a field on [`super::Monitor`], because [`wait`] blocks indefinitely
//! and the monitor's locks are held across a whole gate call -- a waiter parked under one would
//! stop the system it is waiting for.

use std::sync::atomic::{AtomicU64, Ordering};

use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};

static STATE: AtomicU64 = AtomicU64::new(0);

/// Read the current state.
pub fn get() -> u64 {
    STATE.load(Ordering::SeqCst)
}

/// Or `bits` into the state, waking waiters if that changed it. Returns the new value.
pub fn or(bits: u64) -> u64 {
    let old = STATE.fetch_or(bits, Ordering::SeqCst);
    let new = old | bits;
    if new != old {
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&STATE),
                usize::MAX,
            ))],
            None,
        );
    }
    new
}

/// Block until the state differs from `cur`, then return it.
pub fn wait(cur: u64) -> u64 {
    let mut sleep = [ThreadSync::new_sleep(ThreadSyncSleep::new(
        ThreadSyncReference::Virtual(&STATE),
        cur,
        ThreadSyncOp::Equal,
        ThreadSyncFlags::empty(),
    ))];
    let _ = sys_thread_sync(&mut sleep, None);
    get()
}
