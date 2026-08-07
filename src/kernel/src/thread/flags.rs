use core::sync::atomic::Ordering;

use twizzler_abi::upcall::UpcallInfo;

use super::{Thread, current_thread_ref};

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
        thread.kernel_depth.fetch_add(1, Ordering::SeqCst);

        if thread.flags.load(Ordering::SeqCst) & THREAD_MUST_EXIT != 0 {
            // TODO
            super::exit(101);
        }
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
        thread.upcalls_since_user.store(0, Ordering::SeqCst);
        thread.remove_donated_priority();
        if thread.arch.has_upcall_restore_frame() {
            return;
        }
        if thread.secctx.active_id()
            == thread
                .upcall_target
                .lock()
                .map(|ut| ut.self_ctx)
                .unwrap_or(0.into())
        {
            let pending_message = thread.pending_message.swap(0, Ordering::SeqCst);
            if pending_message != 0 {
                thread.send_upcall(UpcallInfo::Mailbox(pending_message));
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

    /// See [`THREAD_ACTIVE_RUNNING`]. Only `set_current_thread` may call these.
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
    #[inline]
    pub fn is_active_running(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_ACTIVE_RUNNING != 0
    }

    pub fn set_mutex_wait(&self, set: bool) {
        if set {
            self.flags.fetch_or(THREAD_MUTEX_WAIT, Ordering::SeqCst);
        } else {
            self.flags.fetch_and(!THREAD_MUTEX_WAIT, Ordering::SeqCst);
        }
    }

    pub fn get_mutex_wait(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_MUTEX_WAIT != 0
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
        assert!(self.mutex_count.load(Ordering::SeqCst) > 0);
        self.mutex_count.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn get_mutex_count(&self) -> u32 {
        self.mutex_count.load(Ordering::SeqCst)
    }
}
