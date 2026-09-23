//! Blocking waits that an interrupting signal handler can break out of.
//!
//! The runtime keeps a per-thread generation word counting handlers that ran on this thread and
//! that POSIX says should interrupt a blocking call. Only libc knows which handlers those are --
//! an ignored signal, or one with `SA_RESTART` set, must leave the call alone -- so libc bumps the
//! word and everything here just observes it.

use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
};
use twizzler_rt_abi::thread::{twz_rt_interrupt_gen, twz_rt_interrupt_word};

/// Sample the calling thread's interrupt generation, once, at the top of a blocking call.
///
/// Sampling per attempt instead would lose a handler that ran between two attempts.
pub(crate) fn interrupt_gen() -> u64 {
    twz_rt_interrupt_gen()
}

/// A sleep operand that is unsatisfied once an interrupting handler has run.
///
/// Handed to `sys_thread_sync` alongside the caller's own operands rather than checked just before
/// it: the call returns as soon as *any* operand is unsatisfied, which is what stops a handler
/// that ran between the caller's check and the sleep from being slept through.
pub(crate) fn sleep_op(intr_gen: u64) -> ThreadSync {
    ThreadSync::new_sleep(ThreadSyncSleep::new(
        ThreadSyncReference::Virtual(twz_rt_interrupt_word()),
        intr_gen,
        ThreadSyncOp::Equal,
        ThreadSyncFlags::empty(),
    ))
}

/// Whether an interrupting handler has run on this thread since `intr_gen` was sampled.
///
/// Check it after an attempt, so available data beats interruption, and before sleeping, so a
/// handler caught during the attempt is not slept through.
pub(crate) fn interrupted_since(intr_gen: u64) -> bool {
    twz_rt_interrupt_gen() != intr_gen
}
