use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU64},
};

use twizzler_abi::thread::ExecutionState;

use crate::{
    arch::processor::spin_wait_iteration,
    instant::Instant,
    spinlock::Spinlock,
    thread::{Thread, current_thread_ref},
};

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

    /// A tracker call was dropped on the floor because there was no current thread, once threading
    /// was up. If this happens between an acquire and its release, the release is silently lost and
    /// the next acquire on that thread trips the "already set" assert.
    pub static NO_CURRENT_THREAD: Counter =
        Counter::new("locktrack call with no current thread (post-threading)");
    /// A spinlock guard was dropped while a different thread was current than at acquisition.
    pub static SPINLOCK_GUARD_CROSSED: Counter = Counter::new("spinlock guard crossed threads");
    /// A scheduler-lock guard was dropped while a different thread was current than at acquisition,
    /// so `enter_critical_unguarded` and `exit_critical` were charged to different threads.
    pub static SCHED_GUARD_CROSSED: Counter = Counter::new("sched guard crossed threads");

    /// A `LockTracker`'s own flag was contended, i.e. two cpus wanted one thread's tracker. Should
    /// be near zero: the only cross-cpu acquirer is `check_timed_out_mutexes` on the bsp.
    pub static TRACKER_CONTENDED: Counter = Counter::new("tracker lock contended");
    /// A tracker could not be acquired, so that piece of bookkeeping was dropped. Losing a record
    /// makes the tracker's view incomplete; corrupting it would make the tracker lie.
    pub static TRACKER_SKIPPED: Counter = Counter::new("tracker unavailable, bookkeeping skipped");
    /// An intent was still set when a new one was recorded. Replaced rather than fatal.
    pub static STALE_INTENT_REPLACED: Counter = Counter::new("stale intent replaced");
    /// A lock was recorded with no matching intent outstanding.
    pub static ORPHAN_RECORD: Counter = Counter::new("lock recorded with no intent");
    /// A thread was torn down still holding tracked locks.
    pub static HELD_LOCKS_AT_EXIT: Counter = Counter::new("tracker freed with locks held");
    /// A mutex was taken while the tracker believed a spinlock was held.
    pub static MUTEX_WITH_SPINLOCK: Counter = Counter::new("mutex taken with spinlock held");
    /// A tracker was unlocked by someone other than its recorded owner -- the direct consequence of
    /// the stall branch above, and the thing that actually corrupts tracker state.
    pub static TRACKER_UNLOCK_BY_NONOWNER: Counter = Counter::new("tracker unlocked by non-owner");

    /// The current thread changed between `intend_to_lock_spinlock` and `record_spinlock_lock`
    /// inside a single `GenericSpinlock::lock`. The intent is then stranded on the outgoing thread
    /// forever -- and the *next* spinlock that thread takes trips "already set". This is the window
    /// `SPINLOCK_GUARD_CROSSED` cannot see, because the guard's thread is captured after the
    /// record.
    pub static INTENT_RECORD_CROSSED: Counter =
        Counter::new("spinlock intent/record crossed thread or cpu");

    /// A thread blocked on something with unbounded latency -- a pager round trip, or a wait for
    /// reclaimed memory -- while holding a sleeping mutex. Every contender for that mutex then
    /// inherits the whole latency of the block, which is what Mode E looks like from outside.
    pub static BLOCK_WITH_MUTEX_HELD: Counter =
        Counter::new("blocked on pager/memory while holding a mutex");

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

    /// A mutex-wait record outlived the wait it described: the thread it belongs to is not in
    /// `Mutex::lock` (it acquired and lost the record, or it died mid-wait). Reported instead of
    /// being counted as a stuck lock -- see `check_timed_out_mutexes`.
    pub static STALE_MUTEX_INTENT: Counter =
        Counter::new("stale mutex intent seen by timeout scan");

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
    /// live and worth designing out; see HANG.md.
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

    /// `warn_if_blocking_with_mutexes` bailed before it could look at anything. Its silence was
    /// being read as "this never happens", which it cannot support: a check that never ran and a
    /// check that ran and found nothing are the same absence of output. This separates them.
    pub static BLOCK_CHECK_SKIPPED: Counter =
        Counter::new("blocking-with-mutex check skipped (no tracker, or incomplete)");
    /// The same check ran and found no mutexes held -- the ordinary case, and the one that makes
    /// the silence meaningful. Counted rather than printed: it is on every pager round trip.
    pub static BLOCK_CHECK_CLEAR: Counter =
        Counter::new("blocking-with-mutex check ran, no mutexes held");

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

    static ALL: [&Counter; 30] = [
        &CRITICAL_LEAK_AT_ENTRY,
        &CRITICAL_LEAK_AT_EXIT,
        &NO_CURRENT_THREAD,
        &SPINLOCK_GUARD_CROSSED,
        &SCHED_GUARD_CROSSED,
        &INTENT_RECORD_CROSSED,
        &TRACKER_CONTENDED,
        &TRACKER_SKIPPED,
        &TRACKER_UNLOCK_BY_NONOWNER,
        &STALE_INTENT_REPLACED,
        &ORPHAN_RECORD,
        &HELD_LOCKS_AT_EXIT,
        &MUTEX_WITH_SPINLOCK,
        &BLOCK_WITH_MUTEX_HELD,
        &STALE_MUTEX_INTENT,
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
        &BLOCK_CHECK_SKIPPED,
        &BLOCK_CHECK_CLEAR,
        &STATE_WAKE_SKIPPED_CRITICAL,
        &STATE_WAKE_SKIPPED_NO_THREAD,
    ];

    /// Token identifying who holds a `LockTracker`'s flag: cpu in the high 16 bits (biased by one
    /// so a valid token is never `NO_OWNER`), thread id in the low 48.
    pub const NO_OWNER: u64 = 0;

    pub fn owner_token() -> u64 {
        let cpu = (this_cpu() as u64).wrapping_add(1) & 0xffff;
        (cpu << 48) | (this_thread() & 0x0000_ffff_ffff_ffff)
    }

    /// `(cpu, thread)` for a token, for printing. Cpu comes back as `u32::MAX` when unknown.
    pub fn split_token(token: u64) -> (u32, u64) {
        let cpu = ((token >> 48) & 0xffff).wrapping_sub(1);
        (cpu as u32, token & 0x0000_ffff_ffff_ffff)
    }

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
        super::current_thread_ref()
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

pub struct LockTrackerInner {
    mutexes: heapless::Vec<Option<Lock>, 16>,
    spinlocks: heapless::Vec<Option<Lock>, 16>,
    intended_to_mutexlock: Option<Lock>,
    /// The one spinlock this thread is currently trying to take.
    ///
    /// Deliberately a single slot, not a stack: taking a spinlock while spinning for another is
    /// always a bug, so a second intent arriving here is a finding, not a case to model. It is how
    /// Mode A was found -- `spin_wait_until` polls TLB shootdowns from inside the spin, and
    /// `TlbShootdownInfo::complete` warned through the console lock from there.
    intended_to_spinlock: Option<Lock>,
    id: u64,
}

pub struct LockTracker {
    inner: Box<UnsafeCell<LockTrackerInner>>,
    lock: AtomicBool,
    id: u64,
    /// DIAG: who holds `lock`, as a `diag::owner_token()`. Reporting only.
    owner: AtomicU64,
    /// Set the first time a piece of bookkeeping is dropped for this thread. From then on the
    /// tracker's view is known-incomplete: a lock it shows as held may have been released, and an
    /// intent it shows may have been satisfied. Anything that draws a *conclusion* from tracker
    /// state has to check this first -- reporting off an incomplete tracker is how Mode E was
    /// manufactured.
    incomplete: AtomicBool,
}

#[derive(Debug)]
pub struct Lock {
    caller: &'static core::panic::Location<'static>,
    locked: bool,
    time: Instant,
    /// Split from the location, and the location kept as a plain address, because this record is
    /// read by threads that do not hold the tracker flag (`print_locks`). A single
    /// `Option<(u64, &'static Location)>` cannot be read that way: the two words are written
    /// separately, so a reader can pass the `Some` check and then observe the pointer half already
    /// replaced or cleared -- and merely *forming* a null `&'static Location` is UB, before
    /// anything formats it.
    owner: Option<u64>,
    /// Address of the owner's `Location`, or 0 for none. See `owner`.
    owner_at: usize,
    /// DIAG: cpu this record was made on. Differing from the observing cpu means the entry
    /// outlived a context switch.
    cpu: u32,
}

impl Lock {
    pub fn caller(&self) -> &'static core::panic::Location<'static> {
        self.caller
    }

    /// The caller address as an integer, without forming the reference the field holds. For
    /// readers without the tracker flag: the owning thread can replace this whole record
    /// (`intend_to_lock_mutex`) or drop it (`clear_intended_mutex`) mid-read.
    pub fn caller_addr(&self) -> usize {
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(self.caller).cast::<usize>()) }
    }

    pub fn owner_id(&self) -> Option<u64> {
        self.owner
    }

    pub fn owner_addr(&self) -> usize {
        self.owner_at
    }

    /// The owner's location, dereferencing `owner_at`. Only sound from a caller holding the
    /// tracker flag -- use `owner_addr` otherwise.
    fn owner_location(&self) -> Option<&'static core::panic::Location<'static>> {
        (self.owner_at != 0)
            .then(|| unsafe { &*(self.owner_at as *const core::panic::Location<'static>) })
    }

    pub fn cpu(&self) -> u32 {
        self.cpu
    }

    pub fn time(&self) -> Instant {
        self.time
    }

    /// Age in milliseconds, or `None` if the record predates a working clock (`Instant::zero()`,
    /// which spinlock intents use).
    fn age_ms(&self, now: Instant) -> Option<u128> {
        now.checked_sub_instant(&self.time).map(|d| d.as_millis())
    }

    pub fn new(caller: &'static core::panic::Location<'static>, time: Instant) -> Self {
        Self {
            caller,
            locked: true,
            time,
            owner: None,
            owner_at: 0,
            cpu: diag::this_cpu(),
        }
    }

    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }
}

impl LockTracker {
    pub fn new(id: u64) -> Self {
        Self {
            inner: Box::new(UnsafeCell::new(LockTrackerInner::new(id))),
            lock: AtomicBool::new(false),
            id,
            owner: AtomicU64::new(diag::NO_OWNER),
            incomplete: AtomicBool::new(false),
        }
    }

    /// False once any bookkeeping for this thread has been dropped; see [`Self::incomplete`].
    pub fn is_complete(&self) -> bool {
        !self.incomplete.load(core::sync::atomic::Ordering::Relaxed)
    }

    fn note_incomplete(&self) {
        self.incomplete
            .store(true, core::sync::atomic::Ordering::Relaxed);
    }

    pub unsafe fn inner(&self) -> &mut LockTrackerInner {
        unsafe { &mut *self.inner.get() }
    }

    /// Acquire this tracker, or give up. Bookkeeping is diagnostic: dropping a record leaves the
    /// tracker's view incomplete, but forcing the lock would let two cpus into one tracker and make
    /// it lie -- which is strictly worse, and is what the old `self.unlock()` here did.
    pub fn lock_or_skip(&self) -> Option<&mut LockTrackerInner> {
        if crate::interrupt::get() {
            diag::TRACKER_SKIPPED.hit();
            self.note_incomplete();
            return None;
        }
        let mut iter = 0;
        while self.lock.swap(true, core::sync::atomic::Ordering::Acquire) {
            if iter == 0 {
                diag::TRACKER_CONTENDED.hit();
            }
            if iter >= 10000 {
                self.note_incomplete();
                if diag::TRACKER_SKIPPED.hit() {
                    let (ocpu, othread) =
                        diag::split_token(self.owner.load(core::sync::atomic::Ordering::Relaxed));
                    emerglogln!(
                        "locktrack: tracker {} busy (cpu {} thread {}), skipping",
                        self.id,
                        ocpu,
                        othread,
                    );
                }
                return None;
            }
            spin_wait_iteration();
            iter += 1;
        }
        self.owner
            .store(diag::owner_token(), core::sync::atomic::Ordering::Relaxed);
        Some(unsafe { &mut *self.inner.get() })
    }

    pub fn try_lock(&self) -> Option<&mut LockTrackerInner> {
        if !self.lock.swap(true, core::sync::atomic::Ordering::Acquire) {
            self.owner
                .store(diag::owner_token(), core::sync::atomic::Ordering::Relaxed);
            Some(unsafe { &mut *self.inner.get() })
        } else {
            None
        }
    }

    pub fn unlock(&self) {
        // DIAG: an unlock from anyone but the recorded owner means the flag no longer protects the
        // inner state. No path does this deliberately any more, so a hit here is a real finding.
        let recorded = self
            .owner
            .swap(diag::NO_OWNER, core::sync::atomic::Ordering::Relaxed);
        let current = diag::owner_token();
        if recorded != current && diag::TRACKER_UNLOCK_BY_NONOWNER.hit() {
            let (ocpu, othread) = diag::split_token(recorded);
            let (ccpu, cthread) = diag::split_token(current);
            emerglogln!(
                "locktrack: tracker {} unlocked by cpu {} thread {}, but owned by cpu {} thread {}",
                self.id,
                ccpu,
                cthread,
                ocpu,
                othread,
            );
        }
        self.lock
            .store(false, core::sync::atomic::Ordering::Release);
    }

    /// Full detail when the flag is free, addresses only when it is not. Previously this always
    /// took `inner()` without the flag, so every caller was reading records their owner could be
    /// rewriting -- which is a kernel fault, not a garbled line, once a `Location` is formatted.
    pub fn print_locks(&self) {
        let int = crate::interrupt::disable();
        match self.try_lock() {
            Some(inner) => {
                inner.print_locks();
                self.unlock();
            }
            None => unsafe { self.inner() }.print_locks_racy(),
        }
        crate::interrupt::set(int);
    }

    pub fn held_locks(&self) -> usize {
        let int = crate::interrupt::disable();
        let count = match self.lock_or_skip() {
            Some(inner) => {
                let count = inner.mutex_count() + inner.spinlock_count();
                self.unlock();
                count
            }
            None => 0,
        };
        crate::interrupt::set(int);
        count
    }

    pub fn mutex_count(&self) -> usize {
        let int = crate::interrupt::disable();
        let count = match self.lock_or_skip() {
            Some(inner) => {
                let count = inner.mutex_count();
                self.unlock();
                count
            }
            None => 0,
        };
        crate::interrupt::set(int);
        count
    }

    pub fn spinlock_count(&self) -> usize {
        let int = crate::interrupt::disable();
        let count = match self.lock_or_skip() {
            Some(inner) => {
                let count = inner.spinlock_count();
                self.unlock();
                count
            }
            None => 0,
        };
        crate::interrupt::set(int);
        count
    }
}

impl LockTrackerInner {
    pub const fn new(id: u64) -> Self {
        Self {
            mutexes: heapless::Vec::new(),
            spinlocks: heapless::Vec::new(),
            intended_to_mutexlock: None,
            intended_to_spinlock: None,
            id,
        }
    }

    pub fn intended_mutex_owned_by(
        &mut self,
        thread_id: u64,
        from: &'static core::panic::Location<'static>,
    ) {
        if let Some(ref mut lock) = self.intended_to_mutexlock {
            lock.owner = Some(thread_id);
            lock.owner_at = from as *const _ as usize;
        }
    }

    pub fn clear_intended_mutex(&mut self) {
        self.intended_to_mutexlock = None;
    }

    pub fn intend_to_lock_mutex(
        &mut self,
        caller: &'static core::panic::Location<'static>,
        time: Instant,
    ) {
        if let Some(stale) = self.intended_to_mutexlock.take() {
            if diag::STALE_INTENT_REPLACED.hit() {
                emerglogln!(
                    "locktrack: tracker {} stale mutex intent {} (cpu {}) replaced by {}",
                    self.id,
                    stale.caller(),
                    stale.cpu(),
                    caller,
                );
            }
        }
        self.intended_to_mutexlock = Some(Lock::new(caller, time));
    }

    pub fn record_mutex_lock(&mut self) -> Option<usize> {
        let Some(lock) = self.intended_to_mutexlock.take() else {
            if diag::ORPHAN_RECORD.hit() {
                emerglogln!(
                    "locktrack: mutex recorded with no intent on tracker {} (cpu {})",
                    self.id,
                    diag::this_cpu(),
                );
            }
            return None;
        };
        let len = self.mutexes.len();
        self.mutexes.push(Some(lock)).ok().map(|_| len)
    }

    pub fn record_mutex_unlock(&mut self, index: usize) {
        if let Some(lock) = self.mutexes.get_mut(index) {
            *lock = None;
        }
        self.try_compact();
    }

    pub fn mutex_count(&self) -> usize {
        self.mutexes
            .iter()
            .filter(|l| l.as_ref().is_some_and(|lock| lock.is_locked()))
            .count()
    }

    pub fn intend_to_lock_spinlock(&mut self, caller: &'static core::panic::Location<'static>) {
        if let Some(stale) = self.intended_to_spinlock.take() {
            // Two acquisitions outstanding on one thread. Either bookkeeping was lost, or -- the
            // Mode A case -- something took a spinlock from inside another's spin, which is never
            // correct. Was fatal; reported since Layer 1, because halting on it destroys the
            // evidence needed to tell those two apart.
            if diag::STALE_INTENT_REPLACED.hit() {
                emerglogln!(
                    "locktrack: tracker {} stale spinlock intent {} (cpu {}) replaced by {} (cpu {})",
                    self.id,
                    stale.caller(),
                    stale.cpu(),
                    caller,
                    diag::this_cpu(),
                );
            }
        }
        self.intended_to_spinlock = Some(Lock::new(caller, Instant::zero()));
    }

    pub fn record_spinlock_lock(&mut self) -> Option<usize> {
        let Some(lock) = self.intended_to_spinlock.take() else {
            if diag::ORPHAN_RECORD.hit() {
                emerglogln!(
                    "locktrack: spinlock recorded with no intent on tracker {} (cpu {})",
                    self.id,
                    diag::this_cpu(),
                );
            }
            return None;
        };
        let len = self.spinlocks.len();
        self.spinlocks.push(Some(lock)).ok().map(|_| len)
    }

    pub fn set_spinlock_locked(&mut self, index: usize, locked: bool) {
        if let Some(lock) = self.spinlocks.get_mut(index) {
            if let Some(lock) = lock {
                lock.set_locked(locked);
            } else {
                log::error!("set_spinlock_locked called on None lock at index {}", index);
            }
        } else {
            log::error!("set_spinlock_locked called with invalid index {}", index);
        }
    }

    pub fn record_spinlock_unlock(&mut self, index: usize) {
        // Clear the slot rather than removing it: guards hold the index they were given at lock
        // time, and locks are not always released in LIFO order (see utils::spinlock_two), so
        // shifting the vector would leave outstanding guards pointing at the wrong entry.
        if let Some(lock) = self.spinlocks.get_mut(index) {
            *lock = None;
        }
        self.try_compact();
    }

    pub fn spinlock_count(&self) -> usize {
        self.spinlocks
            .iter()
            .filter(|l| l.as_ref().is_some_and(|lock| lock.is_locked()))
            .count()
    }

    pub fn holds_no_locks(&self) -> bool {
        self.mutex_count() == 0 && self.spinlock_count() == 0
    }

    fn try_compact(&mut self) {
        while let Some(true) = self.mutexes.last().map(|l| l.is_none()) {
            self.mutexes.pop();
        }
        while let Some(true) = self.spinlocks.last().map(|l| l.is_none()) {
            self.spinlocks.pop();
        }
    }

    pub fn print_locks(&self) {
        self.print_locks_at(None);
    }

    /// `print_locks_at` for a tracker whose flag we could not take, i.e. one its own thread may be
    /// writing right now. Prints locations as addresses and never dereferences one: resolve them
    /// with addr2line against the booted kernel binary, the same way a dump is read.
    ///
    /// The distinction is not academic. The dereferencing version, reached from the idle loop for
    /// a tracker that only *looked* wedged, faulted this kernel at V(0x0) formatting an owner
    /// location that had been cleared between the `Some` check and the read.
    pub fn print_locks_racy(&self) {
        emerglogln!("== LockTracker for thread {} (racy, flag held):", self.id);
        for (i, lock) in self.mutexes.iter().enumerate() {
            if let Some(lock) = lock {
                emerglogln!(
                    "  mutex {}: at {:#x} ({}, cpu {})",
                    i,
                    lock.caller_addr(),
                    if lock.is_locked() {
                        "locked"
                    } else {
                        "unlocked"
                    },
                    lock.cpu(),
                );
            }
        }
        for (i, lock) in self.spinlocks.iter().enumerate() {
            if let Some(lock) = lock {
                emerglogln!(
                    "  spinlock {}: at {:#x} ({}, cpu {})",
                    i,
                    lock.caller_addr(),
                    if lock.is_locked() {
                        "locked"
                    } else {
                        "unlocked"
                    },
                    lock.cpu(),
                );
            }
        }
        if let Some(lock) = self.intended_to_mutexlock.as_ref() {
            emerglogln!(
                "  intend mutex: at {:#x} (owner {:?} at {:#x})",
                lock.caller_addr(),
                lock.owner_id(),
                lock.owner_addr(),
            );
        }
        if let Some(lock) = self.intended_to_spinlock.as_ref() {
            emerglogln!(
                "  intend spinlock: at {:#x} (cpu {})",
                lock.caller_addr(),
                lock.cpu(),
            );
        }
    }

    /// `now`, when given, adds each held mutex's age -- how long this thread has had it. A stuck
    /// lock is only identifiable from a dump if you can tell a lock taken microseconds ago from one
    /// held for seconds.
    pub fn print_locks_at(&self, now: Option<Instant>) {
        emerglogln!("== LockTracker for thread {}:", self.id);
        if self.mutex_count() > 0 {
            emerglogln!("Mutexes held:");
            for (i, lock) in self.mutexes.iter().enumerate() {
                if let Some(lock) = lock {
                    let state = if lock.is_locked() {
                        "locked"
                    } else {
                        "unlocked"
                    };
                    match now.and_then(|now| lock.age_ms(now)) {
                        Some(age) => {
                            emerglogln!("  {}: {} ({}, held {} ms)", i, lock.caller(), state, age)
                        }
                        None => emerglogln!("  {}: {} ({})", i, lock.caller(), state),
                    }
                }
            }
        }
        if self.spinlock_count() > 0 {
            emerglogln!("Spinlocks held:");
            for (i, lock) in self.spinlocks.iter().enumerate() {
                if let Some(lock) = lock {
                    if lock.is_locked() {
                        emerglogln!("  {}: {} (locked)", i, lock.caller());
                    } else {
                        emerglogln!("  {}: {} (unlocked)", i, lock.caller());
                    }
                }
            }
        }

        if let Some(lock) = self.intended_to_mutexlock.as_ref() {
            match (lock.owner_id(), lock.owner_location()) {
                (Some(id), Some(at)) => emerglogln!(
                    "Intend to lock mutex: {} (owned by thread {} at {})",
                    lock.caller(),
                    id,
                    at
                ),
                (Some(id), None) => emerglogln!(
                    "Intend to lock mutex: {} (owned by thread {})",
                    lock.caller(),
                    id
                ),
                _ => emerglogln!("Intend to lock mutex: {}", lock.caller()),
            }
        }

        if let Some(lock) = self.intended_to_spinlock.as_ref() {
            emerglogln!(
                "Intend to lock spinlock: {} (recorded on cpu {})",
                lock.caller(),
                lock.cpu()
            );
        }
    }

    pub fn mutex_wait_time(&self) -> Option<Instant> {
        self.intended_to_mutexlock.as_ref().map(|l| l.time)
    }

    /// The mutex this thread is currently waiting for: where it is being taken, and (if the wait
    /// has seen a holder) who holds it and where they took it.
    pub fn intended_mutex(
        &self,
    ) -> Option<(
        &'static core::panic::Location<'static>,
        Option<(u64, &'static core::panic::Location<'static>)>,
    )> {
        self.intended_to_mutexlock
            .as_ref()
            .map(|l| (l.caller(), l.owner_id().zip(l.owner_location())))
    }

    /// The spinlock this thread is currently trying to take, if any.
    ///
    /// A thread spinning here stays `Running` and records nothing in `intended_to_mutexlock`, so
    /// reading only [`Self::intended_mutex`] reports it as "not waiting" -- which is how a wait
    /// edge through a spinlock disappears from a chain that is otherwise fully traced.
    pub fn intended_spinlock(&self) -> Option<&'static core::panic::Location<'static>> {
        self.intended_to_spinlock.as_ref().map(|l| l.caller())
    }

    fn log_held_mutexes(&self) {
        for lock in self.mutexes.iter().flatten() {
            if lock.is_locked() {
                emerglogln!("    holding mutex from {}", lock.caller());
            }
        }
    }
}

const DISABLE_LOCK_TRACKING: bool = false; // !cfg!(debug_assertions) or test mode;

/// The A/B switch for the whole tracker, `DISABLE_LOCK_TRACKING` read the way call sites want it.
///
/// Everything charged per lock acquisition has to be behind this, not just the `with_tracker`
/// calls: `GenericSpinlock::lock` also resolves the current thread and cpu twice on its own to
/// check for a thread crossing, and those reads are the same order of cost as the bookkeeping they
/// guard. A const so both arms fold at compile time.
#[inline(always)]
pub const fn enabled() -> bool {
    !DISABLE_LOCK_TRACKING
}

static LOCK_TRACKER_CALLS: AtomicU64 = AtomicU64::new(0);

#[track_caller]
pub fn with_lock_tracker<R: Default>(f: impl FnOnce(&mut LockTrackerInner) -> R) -> R {
    if DISABLE_LOCK_TRACKING || in_switch_window() {
        return R::default();
    }
    // After the gate, not before it: ahead of it this says "ENABLED" in a build where tracking is
    // off, and it charges a contended atomic to the arm whose whole point is that it charges
    // nothing.
    if LOCK_TRACKER_CALLS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) == 0 {
        emerglogln!("LOCK TRACKING ENABLED");
    }
    let Some(ct) = current_thread_ref() else {
        // DIAG: this is the silent-drop path. An acquire recorded with a current thread and a
        // release that lands here leaves the acquire outstanding forever. Only interesting once
        // per-cpu state exists; before that, having no current thread is just early boot.
        if diag::threading_up() && diag::NO_CURRENT_THREAD.hit() {
            emerglogln!(
                "locktrack: no current thread at {} (cpu {})",
                core::panic::Location::caller(),
                diag::this_cpu()
            );
        }
        return R::default();
    };
    with_tracker(ct.lock_tracker(), f)
}

/// Report, rate-limited, that the current thread is about to block on `what` -- something with
/// unbounded latency, i.e. a pager round trip or a wait for reclaimed memory -- while holding a
/// sleeping mutex. Diagnostic only; blocking here is legal, but every contender for that mutex now
/// waits for the same thing this thread is waiting for.
pub fn warn_if_blocking_with_mutexes(what: &str) {
    if DISABLE_LOCK_TRACKING {
        return;
    }
    // Both bails below are why this check's silence proves nothing on its own: neither one means
    // "no thread ever blocked holding a mutex", they mean the question was never asked. The idle
    // thread plausibly hits the first, and any thread that has ever lost a record hits the second.
    let Some(tracker) = current_tracker() else {
        if diag::BLOCK_CHECK_SKIPPED.hit() {
            emerglogln!(
                "locktrack: blocking-on-{} check skipped: no current tracker (cpu {})",
                what,
                diag::this_cpu(),
            );
        }
        return;
    };
    if !tracker.is_complete() {
        if diag::BLOCK_CHECK_SKIPPED.hit() {
            emerglogln!(
                "locktrack: blocking-on-{} check skipped: tracker {} incomplete",
                what,
                tracker.id,
            );
        }
        return;
    }
    with_tracker(tracker, |lt| {
        if lt.mutex_count() == 0 {
            // The ordinary case, and the one that makes a zero on BLOCK_WITH_MUTEX_HELD mean
            // something. Counted only -- it is on every pager round trip.
            diag::BLOCK_CHECK_CLEAR.count_only();
            return;
        }
        if !diag::BLOCK_WITH_MUTEX_HELD.hit() {
            return;
        }
        emerglogln!(
            "locktrack: thread {} blocking on {} while holding {} mutex(es):",
            lt.id,
            what,
            lt.mutex_count(),
        );
        lt.log_held_mutexes();
    });
}

/// Per-cpu -- TLS is per-cpu in this kernel -- set while this cpu has installed a thread as current
/// but has not yet won its `switch_lock`.
///
/// In that window the thread is still current on the cpu switching away from it, so both cpus would
/// charge one `LockTracker`: they contend it, `lock_or_skip` drops records, and one cpu's intent is
/// displaced by the other's. That is Mode A's second producer. The register handoff itself is
/// serialized by `switch_lock`, so this is purely an attribution problem, and the window is not
/// worth attributing: the only locks in it are `SecCtxMgr::active_id` and `VirtContext::switch_to`,
/// both short and non-blocking.
#[thread_local]
static IN_SWITCH: core::cell::Cell<bool> = core::cell::Cell::new(false);

/// Called by `sched::switch_to` immediately before it installs the incoming thread as current.
pub fn enter_switch_window() {
    if crate::processor::tls_ready() {
        IN_SWITCH.set(true);
    }
}

/// Called on whichever cpu a thread resumes on, once the switch has completed. Every resume path
/// must call it: the normal one (returning from `switch_thread`) and the entry points a
/// freshly-created thread starts at, which never return through `switch_to`.
pub fn leave_switch_window() {
    if crate::processor::tls_ready() {
        IN_SWITCH.set(false);
    }
}

fn in_switch_window() -> bool {
    crate::processor::tls_ready() && IN_SWITCH.get()
}

/// The tracker to charge work on this cpu to. Callers pairing an acquire with a release capture
/// this once and pass it to [`with_tracker`] for both halves.
pub fn current_tracker() -> Option<&'static LockTracker> {
    if DISABLE_LOCK_TRACKING || in_switch_window() {
        return None;
    }
    current_thread_ref().map(|ct| ct.lock_tracker())
}

/// Run `f` against a specific tracker, rather than whichever thread happens to be current now.
pub fn with_tracker<R: Default>(
    tracker: &LockTracker,
    f: impl FnOnce(&mut LockTrackerInner) -> R,
) -> R {
    if DISABLE_LOCK_TRACKING {
        return R::default();
    }
    let int = crate::interrupt::disable();
    let r = match tracker.lock_or_skip() {
        Some(inner) => {
            let r = f(inner);
            tracker.unlock();
            r
        }
        None => R::default(),
    };
    crate::interrupt::set(int);
    r
}

impl Thread {
    pub fn print_locks(&self) {
        self.lock_tracker.print_locks();
    }
}

unsafe impl Send for LockTracker {}
unsafe impl Sync for LockTracker {}

static ALL_TRACKERS: Spinlock<heapless::Vec<Option<Arc<LockTracker>>, 1024>> =
    Spinlock::new(heapless::Vec::new());

pub fn inner_size() -> usize {
    core::mem::size_of::<LockTrackerInner>()
}

pub fn register_lock_tracker(tracker: Arc<LockTracker>) -> Option<usize> {
    let mut at = ALL_TRACKERS.lock();
    let pos = at.iter().position(|t| t.is_none());
    if let Some(pos) = pos {
        at[pos] = Some(tracker);
        drop(at);
        return Some(pos);
    }
    let len = at.len();
    let result = at.push(Some(tracker));
    drop(at);
    result.ok().map(|_| len)
}

pub fn deregister_lock_tracker(index: usize) {
    let mut at = ALL_TRACKERS.lock();
    if index < at.len() {
        if let Some(t) = at[index].as_ref() {
            let held = t.held_locks();
            if held != 0 && diag::HELD_LOCKS_AT_EXIT.hit() {
                emerglogln!(
                    "locktrack: tracker {} freed with {} locks held",
                    index,
                    held
                );
            }
        }
        at[index] = None;
    }
}

/// Every live thread, as `(id, state, is_inside_Mutex::lock, is_idle)`.
///
/// Taken as a snapshot for two reasons. The first is a lock order, `ALL_THREADS` -> `ALL_TRACKERS`:
/// dropping the last `ThreadRef` inside `remove_thread` runs `Thread::drop`, which deregisters a
/// tracker, so the scan must not hold `ALL_TRACKERS` and reach for `ALL_THREADS`.
///
/// **That order does not currently arise, and the snapshot is kept anyway.** `remove_thread` has a
/// single caller -- `thread::exit`, where the exiting thread is `current_thread_ref` and so is a
/// live local, with `self_reference` holding a second ref that is reclaimed only later in the reap
/// path -- so the ref dropped there is never the last one and `Thread::drop` never runs. It also
/// now drops both refs *outside* its guards. Keeping the snapshot means a future `remove_thread`
/// caller that is not the exiting thread cannot reintroduce the order silently, against a scan
/// that had been simplified on the grounds that the premise was unrealised.
///
/// The second reason is that this runs on the idle thread, where allocating is a bad idea -- hence
/// the fixed capacity, at the price of truncating on a system with more than `MAX_SNAPSHOT`
/// threads.
fn thread_snapshot() -> heapless::Vec<(u64, ExecutionState, bool, bool), MAX_SNAPSHOT> {
    let mut v = heapless::Vec::new();
    crate::processor::sched::with_all_threads(|threads| {
        for thread in threads.iter() {
            if v.push((
                thread.id(),
                thread.get_state(),
                thread.get_mutex_wait(),
                thread.is_idle_thread(),
            ))
            .is_err()
            {
                break;
            }
        }
    });
    v
}

const MAX_SNAPSHOT: usize = 256;

/// Full dumps emitted per boot. Once a thread is wedged this function finds it on every pass of the
/// idle loop -- the transcripts show ~50 -- and both the console writes and the time they take are
/// enough to move the window being investigated. Three is enough to see it settle.
static STUCK_REPORTS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
const MAX_STUCK_REPORTS: u32 = 3;

fn stuck_reports_left() -> bool {
    STUCK_REPORTS.load(core::sync::atomic::Ordering::Relaxed) < MAX_STUCK_REPORTS
}

pub fn check_timed_out_mutexes() {
    if DISABLE_LOCK_TRACKING {
        return;
    }
    let now = Instant::now();
    let threads = thread_snapshot();

    let at = ALL_TRACKERS.lock();
    //emerglogln!("checking {} threads for timed out mutexes", at.len());
    let mut any = false;
    for th in at.iter() {
        let Some(lock_tracker) = th.as_ref() else {
            continue;
        };
        let Some(lt) = lock_tracker.try_lock() else {
            // A tracker nobody can lock is the most interesting thread in the system, not one to
            // pass over. `with_tracker` holds this flag across a few non-blocking lines with
            // interrupts off, so a thread still holding it a second later is wedged inside that
            // window or left it without unlocking -- and in every category-A transcript the thread
            // that owns the wedge is exactly the one whose tracker reads busy. Skipping it also
            // skipped the `any = true` below, which is why the thread-state dump this function
            // exists to produce has never once printed during the failures it was written for.
            if stuck_reports_left() {
                let (ocpu, othread) = diag::split_token(
                    lock_tracker
                        .owner
                        .load(core::sync::atomic::Ordering::Relaxed),
                );
                emerglogln!(
                    "locktrack: tracker {} busy, flag held by cpu {} thread {}; thread state {:?}",
                    lock_tracker.id,
                    ocpu,
                    othread,
                    threads
                        .iter()
                        .find(|(id, ..)| *id == lock_tracker.id)
                        .map(|(_, state, mutex_wait, idle)| (*state, *mutex_wait, *idle)),
                );
                // Racy by construction -- it reads the inner state without the flag. That is the
                // point: a tracker nobody can lock is the thread worth reporting.
                //
                // "A wedged thread is not mutating its own record" used to justify dereferencing
                // what it found. It does not: the flag is also held across a short interrupts-off
                // window in `with_tracker`, so a `try_lock` failure means "transiently busy" at
                // least as often as "wedged", and this branch caught a *Running* thread mid-write
                // and faulted the kernel at V(0x0). `print_locks` now prints addresses when it
                // cannot take the flag.
                lock_tracker.print_locks();
            }
            any = true;
            continue;
        };
        if let Some(waited) = lt.mutex_wait_time().map(|t| (now - t).as_millis())
            && waited > 1000
        {
            // An intent record only means "waiting" if the thread is actually inside
            // `Mutex::lock`. A record that outlived its acquisition -- or one belonging to a
            // thread that died mid-wait, which is what a halted cpu in a kernel panic leaves
            // behind -- otherwise reports as a stuck lock forever, once per pass, naming whoever
            // happened to hold it last. That is all Mode E ever was.
            let live = threads.iter().find(|(id, ..)| *id == lock_tracker.id);
            let waiting = match live {
                Some((_, _, mutex_wait, _)) => *mutex_wait,
                // Absent from a *truncated* snapshot says nothing, so report rather than dismiss.
                None => threads.len() == MAX_SNAPSHOT,
            };
            if !waiting || !lock_tracker.is_complete() {
                if diag::STALE_MUTEX_INTENT.hit() {
                    match (live, lt.intended_mutex()) {
                        (Some((_, state, ..)), Some((caller, _))) => emerglogln!(
                            "locktrack: stale mutex intent on thread {} ({:?}{}), {} ms old, at {}",
                            lock_tracker.id,
                            state,
                            if lock_tracker.is_complete() {
                                ""
                            } else {
                                ", tracker incomplete"
                            },
                            waited,
                            caller,
                        ),
                        _ => emerglogln!(
                            "locktrack: stale mutex intent on dead thread {}, {} ms old",
                            lock_tracker.id,
                            waited,
                        ),
                    }
                }
                lock_tracker.unlock();
                continue;
            }
            match lt.intended_mutex() {
                Some((caller, Some((owner, owner_at)))) => emerglogln!(
                    "Thread {} has waited {} ms for the mutex at {}, held by thread {} ({:?}) taken at {}",
                    lock_tracker.id,
                    waited,
                    caller,
                    owner,
                    threads
                        .iter()
                        .find(|(id, ..)| *id == owner)
                        .map(|(_, state, ..)| *state)
                        .unwrap_or(ExecutionState::Exited),
                    owner_at,
                ),
                Some((caller, None)) => emerglogln!(
                    "Thread {} has waited {} ms for the mutex at {} (no holder seen)",
                    lock_tracker.id,
                    waited,
                    caller,
                ),
                None => emerglogln!(
                    "Thread {} has been waiting on a mutex for {} ms",
                    lock_tracker.id,
                    waited,
                ),
            }
            any = true;
        }
        lock_tracker.unlock();
    }

    if any && STUCK_REPORTS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < MAX_STUCK_REPORTS
    {
        for th in at.iter() {
            let Some(lock_tracker) = th.as_ref() else {
                continue;
            };
            match lock_tracker.try_lock() {
                Some(lt) => {
                    lt.print_locks_at(Some(now));
                    lock_tracker.unlock();
                }
                // Same reasoning as the busy branch above: a tracker that cannot be locked is the
                // one worth reading, so read it anyway rather than printing a line that says only
                // that we did not.
                None => {
                    emerglogln!(
                        "(tracker {} busy, reading without the flag)",
                        lock_tracker.id
                    );
                    lock_tracker.print_locks();
                }
            }
        }
        // A holder that is Sleeping is blocked on something while holding the lock; one that is
        // Running is either on-cpu or spinning. The lock dumps cannot tell those apart, and it is
        // the first question a stuck lock raises.
        emerglogln!("== thread states:");
        for (id, state, mutex_wait, idle) in threads.iter() {
            emerglogln!(
                "  thread {}: {:?}{}{}",
                id,
                state,
                if *mutex_wait {
                    " (waiting on mutex)"
                } else {
                    ""
                },
                if *idle { " (idle)" } else { "" },
            );
        }
        // The counters are otherwise printed only from a panic or from shutdown, and a hang reaches
        // neither -- so every category-A transcript on disk has no counter dump at all, and the
        // probes that exist to make silence meaningful (BLOCK_CHECK_*, TRACKER_*) cannot be read
        // for exactly the failures they were added for.
        diag::print_counters(true);
    }
}

#[cfg(test)]
mod tests {
    use core::panic::Location;

    use super::{LockTrackerInner, diag};

    /// Two outstanding spinlock intents on one thread must be reported, not accommodated.
    ///
    /// Taking a spinlock while spinning for another is always wrong, so the single intent slot is
    /// the detector, and this is the shape it detects: Mode A was `spin_wait_until` polling TLB
    /// shootdowns from inside a spin, where the shootdown code warned through the console lock.
    /// The second acquire displaces the first's intent (`STALE_INTENT_REPLACED`) and the first then
    /// records against nothing (`ORPHAN_RECORD`) -- the 1:1 pairing that named the bug.
    #[twizzler_kernel_macros::kernel_test]
    fn nested_spinlock_intent_is_reported() {
        let stale_before = diag::STALE_INTENT_REPLACED.count();
        let orphan_before = diag::ORPHAN_RECORD.count();

        let mut lt = LockTrackerInner::new(u64::MAX);
        lt.intend_to_lock_spinlock(Location::caller());
        lt.intend_to_lock_spinlock(Location::caller());
        assert_eq!(diag::STALE_INTENT_REPLACED.count(), stale_before + 1);

        assert!(lt.record_spinlock_lock().is_some());
        assert!(lt.record_spinlock_lock().is_none());
        assert_eq!(diag::ORPHAN_RECORD.count(), orphan_before + 1);
    }

    /// The ordinary case, for contrast: one intent, one record, nothing reported.
    #[twizzler_kernel_macros::kernel_test]
    fn unnested_spinlock_intent_is_silent() {
        let stale_before = diag::STALE_INTENT_REPLACED.count();
        let orphan_before = diag::ORPHAN_RECORD.count();

        let mut lt = LockTrackerInner::new(u64::MAX);
        lt.intend_to_lock_spinlock(Location::caller());
        assert!(lt.record_spinlock_lock().is_some());
        assert_eq!(lt.spinlock_count(), 1);

        assert_eq!(diag::STALE_INTENT_REPLACED.count(), stale_before);
        assert_eq!(diag::ORPHAN_RECORD.count(), orphan_before);
    }
}
