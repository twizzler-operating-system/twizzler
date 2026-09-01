use core::sync::atomic::Ordering;

use twizzler_abi::upcall::UpcallInfo;

use super::{Thread, current_thread_ref, locktrack::diag};

/// A thread crossing the user/kernel boundary must not hold a critical count: user code cannot be
/// in a critical section, so a nonzero count at either edge is a leak from some earlier kernel
/// entry. Mode C is that leak surfacing later, at whatever mutex the thread next takes, which never
/// names the path that caused it -- `critical_origin` does.
fn report_leaked_critical(thread: &Thread, when: &str, counter: &diag::Counter) {
    // Only user threads: a kernel thread reaches these hooks through an interrupt or fault, where
    // being critical at depth 0 is legitimate. Kernel and idle threads are exactly the ones with no
    // memory context.
    if thread.memory_context.is_none() || !thread.is_critical() {
        return;
    }
    if !counter.hit() {
        return;
    }
    let count = thread.critical_counter.load(Ordering::SeqCst);
    match thread.critical_origin() {
        Some(loc) => emerglogln!(
            "locktrack: thread {} ({}) is {} with critical count {}, taken off zero at {}",
            thread.id(),
            thread.objid(),
            when,
            count,
            loc,
        ),
        None => emerglogln!(
            "locktrack: thread {} ({}) is {} with critical count {}, origin unknown",
            thread.id(),
            thread.objid(),
            when,
            count,
        ),
    }
}

pub(super) const THREAD_PROC_IDLE: u32 = 1;
pub(super) const THREAD_HAS_DONATED_PRIORITY: u32 = 2;
pub(super) const THREAD_IS_SYNC_SLEEP: u32 = 8;
pub(super) const THREAD_IS_SYNC_SLEEP_DONE: u32 = 16;
pub(super) const THREAD_IS_EXITING: u32 = 32;
pub(super) const THREAD_IS_SUSPENDED: u32 = 64;
pub(super) const THREAD_MUST_SUSPEND: u32 = 128;
pub(crate) const THREAD_MUST_EXIT: u32 = 256;
pub(crate) const THREAD_MUTEX_WAIT: u32 = 512;
pub(crate) const THREAD_TIMED_WAIT: u32 = 1024;
/// This thread's repr id was handed to userspace by `sys_spawn`, so userspace owns the object's
/// lifetime and the kernel must not delete it when the thread dies. See `Thread::drop`.
pub(crate) const THREAD_REPR_USER_OWNED: u32 = 2048;
/// This thread is the current thread of some cpu. Maintained by `set_current_thread`, which is the
/// only place a cpu's current thread changes, so exactly one thread per cpu carries it.
///
/// It exists because a running thread is otherwise indistinguishable from a lost one: `rq.take()`
/// removed it from the run queue, it waits on nothing, and its state is `Running` -- which is
/// precisely what `check_orphan_threads` looks for. Only the cpu running a thread knows it is
/// running it, and `CURRENT_THREAD` is `#[thread_local]`, so a scan on another cpu cannot ask.
pub(crate) const THREAD_ACTIVE_RUNNING: u32 = 4096;

pub fn enter_kernel() {
    if let Some(thread) = current_thread_ref() {
        if thread.kernel_depth.fetch_add(1, Ordering::SeqCst) == 0 {
            report_leaked_critical(&thread, "entering kernel", &diag::CRITICAL_LEAK_AT_ENTRY);
        }

        // Through `maybe_exit`, not the raw flag: this is one of the two places a pending
        // force-exit is polled, and the restrictions the other one honours apply here too. A raw
        // check kills a thread that is mid-gate at its next syscall or fault -- in the callee's
        // security context, holding the callee's locks -- which is exactly what the spawn-stamped
        // home context (`exit_sctx`) defers the exit to avoid. The flag is sticky, so declining
        // here only delays it to the entry after the thread comes home.
        thread.maybe_exit();
    }
}

pub fn exit_kernel() {
    if let Some(thread) = current_thread_ref() {
        // Saturate rather than wrap: an unbalanced exit must not leave the thread looking like
        // it is deeply nested in the kernel, which would wedge it in kernel state forever.
        let prev = thread
            .kernel_depth
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |d| {
                Some(d.saturating_sub(1))
            })
            .unwrap();
        // Only the outermost exit actually returns to userspace. A nested fault unwinding back
        // into a syscall must not run any of the below, or it tears down priority donation and
        // delivers upcalls while the outer kernel entry is still executing.
        if prev > 1 {
            return;
        }
        report_leaked_critical(&thread, "returning to user", &diag::CRITICAL_LEAK_AT_EXIT);
        // Relaxed: a per-thread runaway-upcall counter, touched only by the thread itself (here
        // and in `send_upcall`) and ordered against nothing. `SeqCst` made this an `xchg` on every
        // return to user, almost always writing the zero that was already there.
        thread.upcalls_since_user.store(0, Ordering::Relaxed);
        thread.remove_donated_priority();
        // A suspend requested while this thread was critical is only recorded:
        // `maybe_suspend_self` declines, and `exit_critical` does nothing on the 0 transition, so
        // it waits for a poll point. Returning to user is one -- no kernel lock is held, no drop
        // glue is in flight, and suspending here is what userspace already expects. Interrupts are
        // off at every call site, which is the condition `schedule(REINSERT)` arranges around its
        // own call for the same reason: who is current must not change under us.
        //
        // Suspend only. `maybe_exit` can call `exit()`, which must not run with interrupts masked;
        // `enter_kernel` already polls it, and MUST_EXIT is sticky.
        if thread.must_suspend() {
            thread.maybe_suspend_self();
        }
        if thread.arch.has_upcall_restore_frame() {
            return;
        }
        // Nothing below does anything without a pending mailbox message -- the `fetch_update`
        // fails outright on a zero mask -- and *testing* for one is a single relaxed load, where
        // the two lines below are two spinlock acquisitions: `active_sctx_id` takes the sctx
        // cache's, and `upcall_target` is one itself. Charged to every return to user, to reach a
        // branch that is almost never taken. `must_return_to_user` already tests in this order;
        // this is the same short-circuit on the path that actually runs.
        //
        // A message landing just after this load is not lost: the mask is sticky, and
        // `must_return_to_user` keeps asking for a return to user while any bit is set, so it is
        // delivered at the next one.
        if thread.pending_message.load(Ordering::Relaxed) == 0 {
            return;
        }
        if thread.active_sctx_id()
            == thread
                .upcall_target
                .lock()
                .map(|ut| ut.self_ctx)
                .unwrap_or(0.into())
        {
            // One message per upcall: take only the lowest pending bit and leave the rest
            // for a later return to user, which `must_return_to_user` keeps asking for as
            // long as any bit is set.
            if let Ok(prev) =
                thread
                    .pending_message
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |m| {
                        (m != 0).then(|| m & (m - 1))
                    })
            {
                thread.send_upcall(UpcallInfo::Mailbox(prev.trailing_zeros() as u64));
            }
        }
    }
}

impl Thread {
    #[inline]
    pub fn is_idle_thread(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_PROC_IDLE != 0
    }

    #[inline]
    pub fn is_in_user(&self) -> bool {
        self.kernel_depth.load(Ordering::SeqCst) == 0
    }

    /// Record that this thread's repr id has been handed to userspace, which then owns when the
    /// object dies.
    pub fn set_repr_user_owned(&self) {
        self.flags
            .fetch_or(THREAD_REPR_USER_OWNED, Ordering::SeqCst);
    }

    pub fn is_repr_user_owned(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_REPR_USER_OWNED != 0
    }

    pub fn set_is_exiting(&self) {
        self.flags.fetch_or(THREAD_IS_EXITING, Ordering::SeqCst);
    }

    pub fn is_exiting(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_IS_EXITING != 0
    }

    /// A force-exit is pending against this thread. Sticky: once set, only the thread's own exit
    /// clears it, so a caller that sees it can act at whatever point is safe rather than having to
    /// act right here.
    pub fn must_exit(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_MUST_EXIT != 0
    }

    /// See [`THREAD_ACTIVE_RUNNING`]. Two callers only: `set_current_thread` sets it, and
    /// `switch_thread` clears it for the outgoing thread before `__do_switch` releases that
    /// thread's `switch_lock` -- the flag must go down before the thread becomes takeable, or a cpu
    /// that legitimately picks it up sees it still marked running.
    pub(super) fn set_active_running(&self, set: bool) {
        if set {
            self.flags.fetch_or(THREAD_ACTIVE_RUNNING, Ordering::SeqCst);
        } else {
            self.flags
                .fetch_and(!THREAD_ACTIVE_RUNNING, Ordering::SeqCst);
        }
    }

    /// True while this thread is some cpu's current thread -- on-cpu, or executing the last few
    /// instructions before a switch hands the cpu away.
    ///
    /// False does *not* mean the thread is off its kernel stack: those last few instructions are
    /// `save_extended_state` and `__do_switch`, both of which write through it. Use
    /// [`Thread::has_left_kernel_stack`] for that question.
    #[inline]
    pub fn is_active_running(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_ACTIVE_RUNNING != 0
    }

    /// Record that this thread is blocked acquiring a mutex at `at`, or (`None`) that it is not.
    pub fn set_mutex_wait(&self, at: Option<&'static core::panic::Location<'static>>) {
        match at {
            Some(loc) => {
                // Location first, then the flag. Readers reach the location only after the flag
                // reads set, so this order is what keeps one from finding "waiting, nowhere".
                // `Relaxed` store published by the `Release` on the flag, which is what actually
                // enforces that order -- and is a plain store rather than the `xchg` `SeqCst`
                // compiles to.
                self.mutex_wait_at
                    .store(loc as *const _ as usize, Ordering::Relaxed);
                self.flags.fetch_or(THREAD_MUTEX_WAIT, Ordering::Release);
            }
            // Conditional because this arm runs on *every* acquisition, uncontended ones included,
            // where the flag was never raised and there is nothing to clear.
            None => {
                if self.flags.fetch_and(!THREAD_MUTEX_WAIT, Ordering::AcqRel) & THREAD_MUTEX_WAIT
                    != 0
                {
                    self.mutex_wait_at.store(0, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn get_mutex_wait(&self) -> bool {
        self.flags.load(Ordering::Acquire) & THREAD_MUTEX_WAIT != 0
    }

    /// Where this thread is blocked acquiring a mutex, if it is.
    ///
    /// The wait edge `report_stuck_owner` cannot get from the lock tracker: intents are recorded
    /// only when tracking is compiled in, so with it off every chain stops at the first owner.
    pub fn mutex_wait_at(&self) -> Option<&'static core::panic::Location<'static>> {
        let p = self.mutex_wait_at.load(Ordering::Relaxed);
        (p != 0).then(|| unsafe { &*(p as *const core::panic::Location<'static>) })
    }

    pub fn set_sync_sleep(&self) {
        self.flags.fetch_or(THREAD_IS_SYNC_SLEEP, Ordering::SeqCst);
    }

    pub fn reset_sync_sleep(&self) -> bool {
        let old = self
            .flags
            .fetch_and(!THREAD_IS_SYNC_SLEEP, Ordering::SeqCst);
        (old & THREAD_IS_SYNC_SLEEP) != 0
    }

    pub fn set_sync_sleep_done(&self) {
        self.flags
            .fetch_or(THREAD_IS_SYNC_SLEEP_DONE, Ordering::SeqCst);
    }

    /// Whether this thread still owns its in-flight thread-sync park. A waker's claim consumes
    /// THREAD_IS_SYNC_SLEEP (`reset_sync_sleep`), so reading it clear on the park path means a
    /// wake for this round is already in flight -- committing to block after that point stakes
    /// the thread's liveness on that waker's requeue handoff landing, which is the lost-wake
    /// class the pre-block check exists to close. See `do_sys_thread_sync`.
    pub fn has_sync_sleep(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_IS_SYNC_SLEEP != 0
    }

    pub fn has_sync_sleep_done(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_IS_SYNC_SLEEP_DONE != 0
    }

    pub fn reset_sync_sleep_done(&self) -> bool {
        let old = self
            .flags
            .fetch_and(!THREAD_IS_SYNC_SLEEP_DONE, Ordering::SeqCst);
        (old & THREAD_IS_SYNC_SLEEP_DONE) != 0
    }

    pub fn set_timed_wait(&self, set: bool) {
        if set {
            self.flags.fetch_or(THREAD_TIMED_WAIT, Ordering::SeqCst);
        } else {
            self.flags.fetch_and(!THREAD_TIMED_WAIT, Ordering::SeqCst);
        }
    }

    pub fn has_timed_wait(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_TIMED_WAIT != 0
    }

    pub fn inc_mutex_count(&self) {
        let r = self.mutex_count.fetch_add(1, Ordering::SeqCst);
        if r > 1000 {
            panic!("mutex count exceeded 1000");
        }
    }

    pub fn dec_mutex_count(&self) {
        if self
            .mutex_count
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |c| c.checked_sub(1))
            .is_err()
            && crate::thread::locktrack::diag::MUTEX_COUNT_UNDERFLOW.hit()
        {
            emerglogln!("mutex count decremented at zero on thread {}", self.id());
        }
    }

    pub fn get_mutex_count(&self) -> u32 {
        self.mutex_count.load(Ordering::SeqCst)
    }
}
