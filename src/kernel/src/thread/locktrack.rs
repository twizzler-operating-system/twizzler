/// Counters for lock-bookkeeping anomalies. Observation only, and every report is rate limited so
/// a hot mismatch cannot flood the console and change timing.
pub mod diag {
    use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

    /// Per-site report budget. Counting continues past it; only printing stops.
    const REPORT_BUDGET: u32 = 16;

    pub struct Counter {
        name: &'static str,
        count: AtomicU64,
        reported: AtomicU32,
    }

    impl Counter {
        pub const fn new(name: &'static str) -> Self {
            Self {
                name,
                count: AtomicU64::new(0),
                reported: AtomicU32::new(0),
            }
        }

        /// Count an event, and return whether the caller should print details for it.
        pub fn hit(&self) -> bool {
            self.count.fetch_add(1, Ordering::Relaxed);
            if self.reported.load(Ordering::Relaxed) >= REPORT_BUDGET {
                return false;
            }
            self.reported.fetch_add(1, Ordering::Relaxed) < REPORT_BUDGET
        }

        /// Count without ever asking to print. For probes on paths where the console write itself
        /// would be the intrusive part -- the context switch, principally.
        pub fn count_only(&self) {
            self.count.fetch_add(1, Ordering::Relaxed);
        }

        pub fn count(&self) -> u64 {
            self.count.load(Ordering::Relaxed)
        }

        pub fn name(&self) -> &'static str {
            self.name
        }
    }

    /// Set once any thread has been made current, i.e. once threading is really up. Before that,
    /// having no current thread is just early boot and says nothing -- and there is enough of it to
    /// exhaust a report budget on its own, so the probe below has to skip it.
    static THREADING_UP: AtomicBool = AtomicBool::new(false);

    /// Called from `set_current_thread`. Read-mostly on purpose: an unconditional store would
    /// bounce this cache line between cpus on every context switch.
    pub fn note_threading_up() {
        if !THREADING_UP.load(Ordering::Relaxed) {
            THREADING_UP.store(true, Ordering::Relaxed);
        }
    }

    pub fn threading_up() -> bool {
        THREADING_UP.load(Ordering::Relaxed)
    }

    /// A scheduler-lock guard was dropped while a different thread was current than at acquisition,
    /// so `enter_critical_unguarded` and `exit_critical` were charged to different threads.
    pub static SCHED_GUARD_CROSSED: Counter = Counter::new("sched guard crossed threads");

    /// A thread was made current on one cpu while already current on another. Two cpus then charge
    /// bookkeeping to one tracker, which both contends `lock_or_skip` (dropping records) and lets
    /// one cpu's intent be displaced by the other's -- the cross-cpu producer of Mode A's pairs.
    pub static THREAD_CURRENT_ON_TWO_CPUS: Counter =
        Counter::new("thread made current on a second cpu");

    /// A thread reached `exit` while still linked into some mutex's sleep queue. Nothing unlinks it
    /// -- `exit` clears the scheduler and requeue memberships but has no back-pointer to the mutex
    /// -- so `release` will hand it the lock and every later locker queues behind a dead owner.
    /// This is the probe for the shutdown hang whose dump is either a non-terminating walk of that
    /// queue or every cpu halted behind it.
    pub static EXIT_WHILE_MUTEX_QUEUED: Counter =
        Counter::new("thread exited while queued on a mutex");

    /// A mutex was released while a different thread was current than the one charged with its
    /// `inc_mutex_count`. Since the charge now rides the guard the count still lands on the right
    /// thread, so this is informational -- but it is the condition that used to underflow, and it
    /// names whichever of the switch window, a `None` current thread, or a guard crossing threads
    /// is actually occurring.
    pub static MUTEX_COUNT_CROSSED: Counter =
        Counter::new("mutex charged to one thread, released while another was current");
    /// `dec_mutex_count` ran with the count already at zero. Absorbed rather than fatal: the
    /// count's only consumer gates `cleanup_exited`, so a wrong value defers a cleanup -- it does
    /// not make anything unsafe.
    pub static MUTEX_COUNT_UNDERFLOW: Counter = Counter::new("mutex count decremented at zero");

    /// `maybe_suspend_self` was reached with a `ThreadRef` that is not the current thread. Only the
    /// running thread can suspend itself, so the call is skipped; `THREAD_MUST_SUSPEND` stays set
    /// and the next `schedule(REINSERT)` retries it.
    pub static SUSPEND_SELF_NOT_CURRENT: Counter =
        Counter::new("maybe_suspend_self called on a non-current thread");

    /// A mutex was owned by an exited thread with a handoff pending, i.e. `release` gave it to a
    /// waiter that died before taking it. Reclaimed by the next locker; without that it is a
    /// permanent hang for everyone behind it.
    pub static MUTEX_HANDOFF_TO_DEAD: Counter =
        Counter::new("mutex handed off to a thread that exited");

    /// `maybe_exit` had a force-exit to deliver but the thread held kernel mutexes, so it was left
    /// to a later poll. The alternative is exiting here and leaking every one of them, which is
    /// what produced the shutdown pile-up on `VirtContext::secctx`. A nonzero count is healthy --
    /// it says the deferral is doing work; a count that rises without the thread ever exiting is
    /// not, and would show up as an unkillable thread.
    pub static EXIT_DEFERRED_MUTEX_HELD: Counter =
        Counter::new("force-exit deferred, thread holds mutexes");

    /// `maybe_exit` had a force-exit to deliver but the thread was executing in a security context
    /// other than the one the exit was restricted to -- i.e. inside a cross-compartment call, where
    /// dying would leave the callee's userspace locks held. Healthy while it rises and the thread
    /// eventually exits; a thread that never comes home shows up as an undelivered force-exit in
    /// `check_orphan_threads` instead.
    pub static EXIT_DEFERRED_SCTX: Counter =
        Counter::new("force-exit deferred, thread in another security context");

    /// A `sys_thread_sync` was entered from inside another one's round -- in practice a fault on a
    /// pager-backed page touched by the outer round, which reaches the pager's queue wait. The slot
    /// slab is per-round and not reentrant, so the nested sleep is refused rather than allowed to
    /// trip `reserve`'s assert. Nonzero means the mixing of pager waits and thread-sync waits is
    /// live and worth designing out.
    pub static NESTED_SYNC_SLEEP: Counter = Counter::new("nested sys_thread_sync sleep refused");

    /// `maybe_exit` declined to exit a thread that is mid-`sys_thread_sync`, so its sleep links get
    /// unlinked by the round's own cleanup instead of abandoned. Pairs with
    /// [SLEEP_LINK_LEAKED_AT_EXIT]: this counts the deferrals that keep that one at zero.
    pub static EXIT_DEFERRED_SLEEP_LINKED: Counter =
        Counter::new("force-exit deferred, thread has sleep links");

    /// A `sys_thread_sync` ended with one of its sleep-link slots still linked into some tree. The
    /// next round reuses that slot and `RBTree::insert` panics with "already linked" -- so this
    /// counts the cause, one round before the symptom, and the report names the thread that leaked
    /// it. Nonzero means a sleep site inserted without a matching removal, which is a bug wherever
    /// it happens; zero is the only healthy value.
    pub static SLEEP_LINK_LEAKED: Counter = Counter::new("sleep link still linked at reset");

    /// A thread reached `Thread::exit` with a sleep-link slot still in some object's tree, so
    /// freeing its slab would leave that tree a dangling node. Distinct from
    /// [SLEEP_LINK_LEAKED]: that one fires at the end of a `sys_thread_sync` round, this one on a
    /// path that never gets a next round to notice. Nonzero means the exit path is where the
    /// sleep-tree corruption comes from.
    pub static SLEEP_LINK_LEAKED_AT_EXIT: Counter =
        Counter::new("sleep link still linked at thread exit");

    /// A thread entered the kernel from userspace already holding a critical count, i.e. some
    /// earlier kernel entry leaked one. This is Mode C's cause, caught at the first point after the
    /// leak where the count is provably wrong -- a user thread cannot be critical while running
    /// user code.
    pub static CRITICAL_LEAK_AT_ENTRY: Counter =
        Counter::new("entered kernel from user with a critical count held");
    /// Same check on the way out: the outermost `exit_kernel` is about to return to userspace with
    /// the count nonzero. Fires one syscall earlier than the entry probe and names the syscall that
    /// leaked it, rather than the next one to notice.
    pub static CRITICAL_LEAK_AT_EXIT: Counter =
        Counter::new("returning to user with a critical count held");

    /// `set_state_and_code` reached a transition it would wake for -- a thread going Exited or
    /// Suspended, or any other change of state -- and dropped the wake because the *calling* thread
    /// was critical.
    ///
    /// The gate there tests the caller's criticality, not the target's, and skipping is silent and
    /// final: nothing retries the wake, so every thread joining or waiting on that repr sleeps on a
    /// state change that has already happened. Self-exit cannot reach it (the guard at the top of
    /// `set_state_and_code` panics instead), which leaves the cross-thread transitions --
    /// force_exit and the ChangeState syscall -- as the way in.
    ///
    /// Probe, not a fix: a zero here across a sweep that reproduces the wedge rules this path out,
    /// which is worth more than the argument that it should not happen.
    pub static STATE_WAKE_SKIPPED_CRITICAL: Counter =
        Counter::new("thread state-change wake skipped, caller critical");
    /// Same skip, reached with no current thread at all. Counted only once threading is up, since
    /// before that it is just early boot and says nothing (see [`NO_CURRENT_THREAD`]).
    pub static STATE_WAKE_SKIPPED_NO_THREAD: Counter =
        Counter::new("thread state-change wake skipped, no current thread");

    /// A voluntary block (`SchedFlags::YIELD`) reached `schedule` on a critical thread, so it
    /// returned without switching and the caller resumed believing it slept. With spinlock guards
    /// charging the critical count, this is a lock held across a block reporting itself instead of
    /// hanging -- `critical_origin` names the acquisition site.
    pub static BLOCK_WHILE_CRITICAL: Counter =
        Counter::new("voluntary block skipped, thread critical");

    static ALL: [&Counter; 17] = [
        &CRITICAL_LEAK_AT_ENTRY,
        &CRITICAL_LEAK_AT_EXIT,
        &SCHED_GUARD_CROSSED,
        &THREAD_CURRENT_ON_TWO_CPUS,
        &MUTEX_COUNT_CROSSED,
        &MUTEX_COUNT_UNDERFLOW,
        &SUSPEND_SELF_NOT_CURRENT,
        &MUTEX_HANDOFF_TO_DEAD,
        &EXIT_DEFERRED_MUTEX_HELD,
        &EXIT_DEFERRED_SCTX,
        &EXIT_DEFERRED_SLEEP_LINKED,
        &NESTED_SYNC_SLEEP,
        &SLEEP_LINK_LEAKED,
        &SLEEP_LINK_LEAKED_AT_EXIT,
        &STATE_WAKE_SKIPPED_CRITICAL,
        &STATE_WAKE_SKIPPED_NO_THREAD,
        &BLOCK_WHILE_CRITICAL,
    ];

    /// Cpu we are on, or `u32::MAX` before per-cpu state exists.
    pub fn this_cpu() -> u32 {
        if crate::processor::tls_ready() {
            crate::current_processor().id
        } else {
            u32::MAX
        }
    }

    /// Id of the current thread, or `u64::MAX` if there isn't one.
    pub fn this_thread() -> u64 {
        crate::thread::current_thread_ref()
            .map(|t| t.id())
            .unwrap_or(u64::MAX)
    }

    /// Printed alongside every kernel panic, so a panicking run always says whether any of the
    /// attribution hazards above actually occurred before it died.
    ///
    /// `always` forces the report even when every counter is zero. Shutdown passes it: a run that
    /// finishes cleanly has to state on the record that the counters were zero, since silence there
    /// is indistinguishable from a build without the instrumentation.
    pub fn print_counters(always: bool) {
        if !always && ALL.iter().all(|c| c.count() == 0) {
            return;
        }
        emerglogln!("== locktrack diagnostics:");
        for c in ALL {
            emerglogln!("  {}: {}", c.name(), c.count());
        }
    }
}
