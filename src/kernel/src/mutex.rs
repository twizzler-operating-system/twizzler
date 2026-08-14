//! Implementation of a mutex that sleeps threads when there is contention.
//!
//! When a mutex's lock function is called, it first tries to wait a bit to see if the mutex frees
//! up, after which it will put the calling thread to sleep. When the current owner of the mutex
//! calls the unlock function, a sleeping thread is chosen and rescheduled.
//!
//! *NOTE*: Because mutexes may sleep threads, mutexes may not be used in critical contexts, such as
//! critical sections or interrupt context.
//!
//! Mutexes interact with the scheduler to perform priority forwarding so that if a high priority
//! thread comes in and sleeps on a mutex owned by a lower priority thread, that lower priority
//! thread will temporarily run with the priority of the thread that just called lock(). In general,
//! a thread that holds a mutex will run with the highest of the priorities of all threads sleeping
//! on that mutex.

// TODO: reenable priority donation, and make it cheaper.

use core::{
    cell::UnsafeCell,
    panic::Location,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use intrusive_collections::{LinkedList, intrusive_adapter};
use twizzler_abi::{syscall::LockStats, thread::ExecutionState};

use crate::{
    idcounter::StableId,
    instant::Instant,
    once::Once,
    processor::{mp::all_processors, sched::schedule_thread},
    spinlock::Spinlock,
    syscall::sync::finish_blocking,
    thread::{
        Thread, ThreadRef, current_thread_ref,
        locktrack::{self, with_lock_tracker},
        priority::Priority,
    },
    time::TimeStatCollector,
};

/// Iterations between repeats of the stuck-wait report. The first fires at 1000; after that a
/// sleeping waiter loops once per wakeup while an idle thread spins continuously, so this is sized
/// for the spinner to report on the order of seconds rather than flood.
const PAUSE_REPORT_EVERY: usize = 5_000_000;

/// Minimum wall-clock gap between repeats, which is what actually bounds the rate for a sleeping
/// waiter -- see the `is_spinner` comment in `lock`.
const PAUSE_REPORT_AFTER: Duration = Duration::from_secs(2);

/// Report an unresolved wait *from the waiter*, which is the one thread guaranteed to still be
/// running when it matters.
///
/// The owner's own wait edge is the part nothing else records. `check_timed_out_mutexes` prints it,
/// but it runs only from the bsp idle loop -- and reaping exited threads is what takes these locks,
/// so that thread is routinely one of the stuck ones. Every stuck-mutex transcript so far has named
/// an owner and nothing whatsoever about it, which is exactly one edge short of a cycle.
///
/// `try_lock`, never block: this runs inside `lock`'s wait loop, where anything that can block
/// wedges the cpu with interrupts masked.
fn report_stuck_owner(caller: &Location<'static>, iters: usize, owner: &ThreadRef) {
    let tracker = owner.lock_tracker();
    let sampled = match tracker.try_lock() {
        // Both accessors yield only ids and 'static locations, so nothing borrows the tracker past
        // this point.
        Some(inner) => {
            let intent = (inner.intended_mutex(), inner.intended_spinlock());
            tracker.unlock();
            Some(intent)
        }
        None => None,
    };
    // 0 for "no current thread": `IdCounter` asserts ids are non-zero, so it cannot collide.
    let waiter = current_thread_ref().map(|t| t.id()).unwrap_or(0);
    let this_cpu = locktrack::diag::this_cpu();
    // `ExecutionState::Running` covers both on-cpu and merely-runnable, and for a lock that is
    // never released those are entirely different bugs: a runnable owner is not being scheduled
    // (a spinner with interrupts masked cannot take a tick to reschedule), an on-cpu one is
    // looping inside its critical section.
    let on_cpu = if owner.is_active_running() {
        "on-cpu"
    } else {
        "runnable/off-cpu"
    };
    // Run queues are per-cpu, so a runnable owner queued on *this* cpu cannot be rescheduled by any
    // other: this loop spins with interrupts masked, so the cpu it is on takes no tick and reaches
    // neither `schedule()` nor `requeue_all()`. `rq == this cpu` is that wedge, stated outright
    // rather than inferred from `runnable/off-cpu`.
    let rq = owner.sched.current_cpu_rq().map(|c| c as i64).unwrap_or(-1);
    // Set by `lock` itself, not by the tracker, so it survives a dropped intent record. Disagreeing
    // with the tracker edge below means the tracker is lying about this owner, not that the owner
    // is running freely -- which is the one way "not waiting on a mutex" can hide a cycle.
    let mutex_wait = owner.get_mutex_wait();
    let complete = if tracker.is_complete() {
        "complete"
    } else {
        "INCOMPLETE"
    };
    // One console write, so a second cpu reporting concurrently cannot split this line in half.
    macro_rules! stall {
        ($edge:literal $(, $arg:expr)* $(,)?) => {
            emerglogln!(
                concat!(
                    "mutex stall: t{} (cpu {}) waited {} at {}; owner t{} ({:?}, {}, rq {}, ",
                    "idle {}, mutex_wait {}, tracker {}) ",
                    $edge,
                ),
                waiter,
                this_cpu,
                iters,
                caller,
                owner.id(),
                owner.get_state(),
                on_cpu,
                rq,
                owner.is_idle_thread(),
                mutex_wait,
                complete,
                $($arg,)*
            )
        };
    }
    // An idle-thread owner cannot be reported like any other: `do_schedule` never reinserts an idle
    // thread on a run queue (`rq -1` is its normal state), so it resumes only when its own cpu next
    // finds nothing else to run. Whether that ever happens is a property of *that cpu*, and nothing
    // in the line above says which cpu it is or what it is doing.
    //
    // All of this reads plain atomics -- `is_idle`, the run-queue flags/load word, and the free-
    // running stat counters -- so it honours the wait loop's "nothing that takes another lock"
    // rule. The reports repeat, so successive snapshots are a time series: a `switches` count that
    // climbs says the owner's cpu is churning through work and simply never reaching its idle
    // thread; one that stands still says the cpu is stuck somewhere of its own.
    if owner.is_idle_thread() {
        let owner_cpu = all_processors()
            .iter()
            .flatten()
            .find(|p| {
                p.idle_thread
                    .poll()
                    .is_some_and(|idle| idle.objid() == owner.objid())
            })
            .map(|p| p.id as i64)
            .unwrap_or(-1);
        emerglogln!(
            "mutex stall:   owner t{} is the idle thread of cpu {}",
            owner.id(),
            owner_cpu
        );
        for p in all_processors().iter().flatten() {
            emerglogln!(
                "mutex stall:   cpu {}{}: idle {} rq_empty {} load {} switches {} steals {} preempts {} hardticks {}",
                p.id,
                if p.id as i64 == owner_cpu {
                    " (owner)"
                } else {
                    ""
                },
                p.is_idle(),
                p.rq_is_empty(),
                p.current_load(),
                p.stats.switches.load(Ordering::Relaxed),
                p.stats.steals.load(Ordering::Relaxed),
                p.stats.preempts.load(Ordering::Relaxed),
                p.stats.hardticks.load(Ordering::Relaxed),
            );
        }
    }
    match sampled {
        Some((Some((at, Some((next, next_at)))), _)) => stall!(
            "is itself waiting at {} for a mutex held by t{} taken at {}",
            at,
            next,
            next_at,
        ),
        Some((Some((at, None)), _)) => stall!("is itself waiting at {}, holder unknown", at),
        // A spinlock edge keeps the owner `Running` and leaves `intended_to_mutexlock` empty, so
        // without this arm it reads identically to an owner that is waiting for nothing at all.
        Some((None, Some(at))) => stall!("is spinning for the spinlock at {}", at),
        // `mutex_wait` is set by `lock` itself and cleared only on acquisition, so the two
        // disagreeing means the owner *is* in a mutex wait whose intent record was lost, and the
        // chain continues past a point this report cannot name. Called out rather than left to
        // read as "waiting for nothing", which is what made one observed chain ambiguous.
        Some((None, None)) if mutex_wait => stall!(
            "is in a mutex wait with no intent recorded -- edge unknown, chain is longer than shown",
        ),
        Some((None, None)) => stall!("is not waiting on a mutex or a spinlock"),
        None => stall!("tracker busy"),
    }
}

#[repr(align(64))]
struct AlignedAtomicU64(AtomicU64);
struct SleepQueue {
    queue: LinkedList<MutexLinkAdapter>,
    pri: Option<Priority>,
    owner: Option<ThreadRef>,
    owned: bool,
    /// When true, release() has handed ownership to a waiter but that waiter
    /// hasn't called lock() yet. Prevents the releasing thread from re-acquiring
    /// and starving waiters (Bug #9 fairness fix).
    handoff: bool,
}

impl SleepQueue {
    /// Remove and return the highest-priority waiter, breaking ties in arrival order so that
    /// equal-priority waiters keep FIFO behavior.
    fn pop_highest_priority(&mut self) -> Option<ThreadRef> {
        // Recomputed from the list rather than read from `pri`: a donation can raise a waiter's
        // effective priority after `pri` was last written.
        let best = self.queue.iter().map(|t| t.effective_priority()).max()?;
        let mut cursor = self.queue.front_mut();
        loop {
            let take = match cursor.get() {
                Some(t) => t.effective_priority() >= best,
                None => break,
            };
            if take {
                return cursor.remove();
            }
            cursor.move_next();
        }
        // A donation raced the scan above, so nothing matched the snapshot. Fall back to FIFO:
        // returning None here would mark the mutex unowned while waiters are still queued,
        // stranding them asleep forever.
        self.queue.pop_front()
    }
}

struct MutexStatCollector {
    nr_locks: usize,
    lock_time: TimeStatCollector,
    hold_time: TimeStatCollector,
}

static MUTEX_STATS: Once<Spinlock<MutexStatCollector>> = Once::new();

/// Whether acquisitions are timed and recorded.
///
/// Off until someone reads [`get_lock_stats`]. Every mutex in the kernel used to charge, per
/// lock/unlock pair, three `Instant::now()` calls and *two acquisitions of one global spinlock* --
/// so every cpu serialized on one cache line to record that it had taken a lock, on a path the
/// fault handler crosses four times. Same shape as the syscall path's `TIMING_ON` and the per-cpu
/// fault counters: cheap to leave available, not cheap to leave on.
static TIMING_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[inline]
fn timing_on() -> bool {
    TIMING_ON.load(Ordering::Relaxed)
}

fn get_mutex_stats() -> &'static Spinlock<MutexStatCollector> {
    MUTEX_STATS.call_once(|| {
        Spinlock::new(MutexStatCollector {
            nr_locks: 0,
            lock_time: TimeStatCollector::new(),
            hold_time: TimeStatCollector::new(),
        })
    })
}

fn add_hold_time_sample(sample: Duration) {
    get_mutex_stats().lock().hold_time.add_sample(sample.into());
}

fn add_lock_time_sample(sample: Duration) {
    let mut stats = get_mutex_stats().lock();
    stats.nr_locks += 1;
    stats.lock_time.add_sample(sample.into());
}

pub fn get_lock_stats() -> LockStats {
    // Asking for the stats is what turns their collection on; see `TIMING_ON`.
    TIMING_ON.store(true, Ordering::Relaxed);
    let stats = get_mutex_stats().lock();
    LockStats {
        mutex_lock_count: stats.nr_locks,
        mutex_waiting_count: 0,
        mutex_avg_waiting_time: stats.lock_time.get_stats(),
        mutex_hold_time: stats.hold_time.get_stats(),
    }
}

intrusive_adapter!(pub MutexLinkAdapter = ThreadRef: Thread { mutex_link: intrusive_collections::linked_list::AtomicLink });

/// A container data structure to manage mutual exclusion.
pub struct Mutex<T> {
    queue: Spinlock<SleepQueue>,
    cell: UnsafeCell<T>,
    locked_at: UnsafeCell<&'static Location<'static>>,
    safe_with_spinlocks: bool,
}

impl<T> Mutex<T> {
    /// Create a new mutex, moving data `T` into it.
    pub const fn new(data: T) -> Self {
        Self {
            queue: Spinlock::new(SleepQueue {
                queue: LinkedList::new(MutexLinkAdapter::NEW),
                pri: None,
                owner: None,
                owned: false,
                handoff: false,
            }),
            cell: UnsafeCell::new(data),
            locked_at: UnsafeCell::new(Location::caller()),
            safe_with_spinlocks: false,
        }
    }

    pub fn set_safe_with_spinlocks(&mut self, safe: bool) {
        self.safe_with_spinlocks = safe;
    }

    /// Get a mut reference to the contained data. Does not perform locking, but is safe because we
    /// have a mut reference to the mutex itself.
    pub fn get_mut(&mut self) -> &mut T {
        self.cell.get_mut()
    }

    /// Lock the mutex and return a lock guard to manage a reference to the managed data. When the
    /// lock guard goes out of scope, the lock will be released.
    #[track_caller]
    pub fn lock(&self) -> LockGuard<'_, T> {
        let timing = timing_on();
        // The tracker stamps its records with this too, so read the clock when either wants it.
        // Both tests fold to nothing when tracking is compiled out and timing is off.
        let start_time = if locktrack::enabled() || timing {
            Instant::now()
        } else {
            Instant::zero()
        };
        let caller = core::panic::Location::caller();
        let current_thread = current_thread_ref();
        let current_donated_priority = current_thread
            .as_ref()
            .and_then(|t| t.get_donated_priority());

        if let Some(ref current_thread) = current_thread {
            if current_thread.is_critical() {
                // The count is almost never this call site's doing -- Mode C is a leaked
                // enter_critical surfacing at whichever mutex the thread takes next -- so name the
                // caller that took the counter off zero, which is the one worth reading.
                panic!(
                    "cannot lock mutex in critical context (called from {}), thread {}, count {}, critical since {}",
                    caller,
                    current_thread.id(),
                    current_thread.critical_counter.load(Ordering::SeqCst),
                    current_thread
                        .critical_origin()
                        .map(|l| l as &dyn core::fmt::Display)
                        .unwrap_or(&"<unknown>"),
                );
            }
            assert!(!current_thread.is_critical());
            assert!(
                !current_thread.mutex_link.is_linked(),
                "cannot lock mutex while waiting for another mutex (called from {}), ct = {}, owner = {}",
                caller,
                current_thread.id(),
                self.queue.lock().locker
            );
            current_thread.inc_mutex_count();
        }
        // The thread charged with the count above. `release` decrements *this* thread rather than
        // whoever is current at drop time: those are two independent resolutions of
        // `current_thread_ref()`, and they diverge across the switch window (where a thread is
        // current on two cpus at once), when one side has no current thread at all, and whenever a
        // guard crosses threads. Same reason `tracker_index` rides the guard.
        let charged = current_thread.cloned();

        // Once a tracker has dropped any bookkeeping its held-lock list can name locks that were
        // released, so the check below would be reporting on a record, not on reality.
        let tracker_trustworthy = locktrack::current_tracker().is_none_or(|t| t.is_complete());
        with_lock_tracker(|lt| {
            // Derived from tracker state, which can be incomplete (see locktrack::diag), so this
            // reports rather than halts. The hazard it names -- sleeping on a mutex while holding a
            // spinlock -- is real, and now shows up as a hang the harness catches instead.
            if !self.safe_with_spinlocks
                && tracker_trustworthy
                && lt.spinlock_count() != 0
                && locktrack::diag::MUTEX_WITH_SPINLOCK.hit()
            {
                emerglogln!(
                    "locktrack: mutex locked at {} while holding a spinlock",
                    caller
                );
                lt.print_locks();
            }
            lt.intend_to_lock_mutex(caller, start_time)
        });

        let int_state = crate::interrupt::disable();
        let mut i = 0;
        let mut stuck_owner: Option<ThreadRef> = None;
        // An idle thread spins here; every other thread loops once per wakeup. `i` therefore
        // measures wildly different things for the two, and a purely iteration-based repeat means a
        // sleeping waiter reports once at 1000 and never again -- so the one transcript that caught
        // a sleeping owner caught it at 1000 iterations, describing a moment rather than the wedge.
        // Repeat on elapsed time instead, reading the clock every iteration only for the sleeper
        // (where a clock read is lost against a sleep/wake round trip) and rarely for the spinner.
        // Set by the first report, at i == 1000, before any arm below consults it.
        let mut last_report = Instant::zero();
        let is_spinner = current_thread
            .as_ref()
            .map(|t| t.is_idle_thread())
            .unwrap_or(true);
        loop {
            i += 1;
            if i == 1000 {
                emerglogln!(
                    "mutex pause: {:?}: {} ({:?})",
                    caller,
                    i,
                    current_thread.as_ref().map(|t| t.is_idle_thread())
                );
                current_thread_ref().map(|ct| ct.print_locks());
            }
            // Nothing may be called from here that takes another lock. This loop runs with
            // interrupts disabled for its whole duration, and the calling thread is mid-acquisition
            // of `self` -- so acquiring a second mutex here either trips `lock`'s own "cannot lock
            // mutex while waiting for another mutex" assert or wedges the cpu with interrupts
            // masked. `check_timed_out_requests()` used to run here and does exactly that
            // (`inflight_mgr().lock()`); it is a timeout sweep the idle thread already drives, so
            // a contended waiter is the wrong place to drive it from. `check_timed_out_mutexes()`
            // above was commented out for the same reason.
            let guard = current_thread.as_ref().map(|ct| ct.enter_critical());
            let _reinsert = {
                let mut queue = self.queue.lock();
                if !queue.owned {
                    queue.owned = true;
                    if let Some(ref thread) = current_thread {
                        if let Some(ref pri) = queue.pri {
                            thread.donate_priority(pri.clone());
                        }
                    }

                    queue.owner = current_thread.cloned();
                    unsafe { self.locked_at.get().write(caller) };
                    break;
                } else if let Some(ref cur_owner) = queue.owner {
                    with_lock_tracker(|lt| {
                        lt.intended_mutex_owned_by(cur_owner.id(), unsafe {
                            self.locked_at.get().read()
                        });
                    });
                    // Sampled here where the owner is in hand, reported after the queue lock is
                    // dropped -- emerglogln takes no lock, but holding this one across a console
                    // write is a needless widening.
                    let due = i == 1000
                        || i % PAUSE_REPORT_EVERY == 0
                        || (i > 1000
                            && (!is_spinner || i % 4096 == 0)
                            && Instant::now()
                                .checked_sub_instant(&last_report)
                                .is_some_and(|d| d >= PAUSE_REPORT_AFTER));
                    if due {
                        last_report = Instant::now();
                        stuck_owner = Some(cur_owner.clone());
                    }
                    if let Some(ref cur_thread) = current_thread {
                        // Compare by objid, not `id()`: the latter comes from `IdCounter`, which
                        // recycles, so a stale owner naming a dead thread whose id was handed to
                        // the caller would read as re-entrancy. objid is the thread's control
                        // object and is never reused.
                        if cur_thread.objid() == cur_owner.objid() {
                            if queue.handoff {
                                // This thread was handed ownership by release(). Clear
                                // the handoff flag and proceed as the new owner.
                                queue.handoff = false;
                                unsafe { self.locked_at.get().write(caller) };
                                break;
                            }
                            crate::panic::backtrace(false, None);
                            // Two hypotheses reach this line and the message has to separate
                            // them: the thread really did lock twice, or `lock()` recorded the
                            // wrong owner because `current_thread_ref()` named the wrong thread
                            // at acquisition. In the second the "owner" never ran the acquiring
                            // code, so it is not on the wait list and `locked_at` names a site
                            // this thread has not reached in this call.
                            panic!(
                                "this mutex is not re-entrant: thread {} ({}) at {}, owner recorded at {} (owner state {:?}, on wait list: {}, this thread on wait list: {}, owned {}, handoff {})",
                                cur_thread.id(),
                                cur_thread.objid(),
                                caller,
                                unsafe { self.locked_at.get().read() },
                                cur_owner.get_state(),
                                cur_owner.mutex_link.is_linked(),
                                cur_thread.mutex_link.is_linked(),
                                queue.owned,
                                queue.handoff,
                            );
                        }
                    }
                    // `release` hands ownership straight to a waiter rather than unlocking, so a
                    // handoff target that dies before consuming it (force-exit, for one) leaves
                    // the mutex owned forever by a thread that will never take it, and every
                    // later locker sleeps behind it. Nothing else can notice: there is no
                    // back-pointer from a thread to the mutexes handed to it, so the exit path
                    // cannot clean this up. Reclaim it here instead.
                    if queue.handoff && cur_owner.get_state() == ExecutionState::Exited {
                        if locktrack::diag::MUTEX_HANDOFF_TO_DEAD.hit() {
                            emerglogln!(
                                "reclaiming mutex handed off to exited thread {} (locked at {})",
                                cur_owner.id(),
                                unsafe { self.locked_at.get().read() }
                            );
                        }
                        queue.handoff = false;
                        queue.owner = current_thread.cloned();
                        unsafe { self.locked_at.get().write(caller) };
                        break;
                    }
                }

                let mut reinsert = true;
                if let Some(thread) = current_thread {
                    thread.set_mutex_wait(true);
                    if !thread.is_idle_thread() {
                        thread.set_state(ExecutionState::Sleeping);
                        queue.queue.push_back(thread.clone());
                        reinsert = false;
                        queue.pri = queue.queue.iter().map(|t| t.effective_priority()).max();
                        if let Some(ref owner) = queue.owner {
                            if let Some(ref pri) = queue.pri {
                                if pri > &owner.effective_priority() {
                                    owner.donate_priority(pri.clone());
                                }
                            }
                        }
                    }
                }
                reinsert
            };
            if let Some(owner) = stuck_owner.take() {
                report_stuck_owner(caller, i, &owner);
            }
            crate::arch::processor::spin_wait_iteration();
            if let Some(guard) = guard {
                finish_blocking(guard);
                if let Some(ct) = current_thread_ref() {
                    assert!(ct.get_mutex_wait());
                    assert!(!ct.mutex_link.is_linked());
                }
            }
        }

        if let Some(ct) = current_thread_ref() {
            assert!(!ct.mutex_link.is_linked());
            ct.set_mutex_wait(false);
        }

        if timing {
            add_lock_time_sample(Instant::now() - start_time);
        }
        let tracker_index = with_lock_tracker(|lt| lt.record_mutex_lock());
        crate::interrupt::set(int_state);
        LockGuard {
            lock: self,
            prev_donated_priority: current_donated_priority,
            start_time,
            timed: timing,
            tracker_index,
            charged,
        }
    }

    fn release(&self, charged: Option<&ThreadRef>) {
        let mut queue = self.queue.lock();

        let g = current_thread_ref().map(|ct| ct.enter_critical());

        if let Some(thread) = queue.pop_highest_priority() {
            // Hand off ownership directly to the next waiter instead of releasing.
            // This prevents the current thread from immediately re-acquiring and
            // starving waiters (Bug #9 fairness fix).
            queue.owner = Some(thread.clone());
            queue.handoff = true;
            // queue.owned stays true
            // Transfer the pending priority donation to the new owner, so it starts
            // running at the correct priority immediately. This prevents priority
            // inversion between schedule_thread() and the new owner's lock() call.
            if let Some(ref pri) = queue.pri {
                thread.donate_priority(pri.clone());
            }
            schedule_thread(thread);
            // Recalculate queue.pri for remaining waiters after popping.
            queue.pri = if queue.queue.is_empty() {
                None
            } else {
                Some(
                    queue
                        .queue
                        .iter()
                        .map(|t| t.effective_priority())
                        .max()
                        .unwrap(),
                )
            };
        } else {
            queue.owner = None;
            queue.owned = false;
            queue.handoff = false;
            queue.pri = None;
        }
        drop(queue);
        if let Some(ct) = charged {
            let cur = current_thread_ref().map(|c| c.id());
            if cur == Some(ct.id()) {
                assert!(!ct.mutex_link.is_linked());
            } else if locktrack::diag::MUTEX_COUNT_CROSSED.hit() {
                // Deliberately not an assert: this is the case that used to underflow, and the
                // point of charging `ct` is that it is now harmless. `mutex_link` is only known
                // quiet for the thread doing the releasing, so the check above stays scoped to it.
                emerglogln!(
                    "mutex charged to thread {} released while {:?} was current (cpu {})",
                    ct.id(),
                    cur,
                    locktrack::diag::this_cpu()
                );
            }
            ct.dec_mutex_count();
        }
        drop(g);
    }
}

/// Manages a reference to the data controlled by a mutex.
#[must_use = "a dropped guard releases immediately; bind it to a variable"]
pub struct LockGuard<'a, T> {
    lock: &'a Mutex<T>,
    prev_donated_priority: Option<Priority>,
    start_time: Instant,
    /// Whether `start_time` is a real reading. Carried rather than re-tested at drop, so a reader
    /// turning timing on mid-hold cannot record a hold time measured from boot.
    timed: bool,
    tracker_index: Option<usize>,
    /// Thread charged with `inc_mutex_count` at acquisition, decremented at release regardless of
    /// who is current then.
    charged: Option<ThreadRef>,
}

impl<T> core::ops::Deref for LockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.cell.get() }
    }
}

impl<T> core::ops::DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.cell.get() }
    }
}

impl<T> Drop for LockGuard<'_, T> {
    fn drop(&mut self) {
        if let Some(ref prev) = self.prev_donated_priority {
            if let Some(thread) = current_thread_ref() {
                thread.remove_donated_priority();
                thread.donate_priority(prev.clone());
            }
        } else if let Some(thread) = current_thread_ref() {
            thread.remove_donated_priority();
        }
        if let Some(index) = self.tracker_index {
            with_lock_tracker(|lt| lt.record_mutex_unlock(index));
        }
        self.lock.release(self.charged.as_ref());
        if self.timed {
            add_hold_time_sample(Instant::now() - self.start_time);
        }
    }
}

unsafe impl<T> Send for Mutex<T> where T: Send {}
unsafe impl<T> Sync for Mutex<T> where T: Send {}
unsafe impl<T> Send for LockGuard<'_, T> where T: Send {}
unsafe impl<T> Sync for LockGuard<'_, T> where T: Send + Sync {}

impl<T> PartialEq for Mutex<T>
where
    T: StableId,
{
    fn eq(&self, other: &Self) -> bool {
        unsafe { (&*self.cell.get()).id() == (&*other.cell.get()).id() }
    }
}

impl<T> Eq for Mutex<T> where T: StableId {}

impl<T> PartialOrd for Mutex<T>
where
    T: StableId,
{
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        unsafe {
            (&*self.cell.get())
                .id()
                .partial_cmp((&*other.cell.get()).id())
        }
    }
}

impl<T> Ord for Mutex<T>
where
    T: StableId,
{
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        unsafe { (&*self.cell.get()).id().cmp((&*other.cell.get()).id()) }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

mod test {
    use alloc::{sync::Arc, vec::Vec};
    use core::{cmp::max, time::Duration};

    use twizzler_kernel_macros::kernel_test;

    use super::Mutex;
    use crate::{
        processor::mp::NR_CPUS,
        spinlock::Spinlock,
        syscall::sync::sys_thread_sync,
        thread::{entry::run_closure_in_new_thread, priority::Priority},
        utils::quick_random,
    };

    /// How long a batch of workers gets before we call it wedged. A batch is at most a handful of
    /// threads doing `INNER_ITER` short acquisitions with the occasional 1ms sleep between them, so
    /// it finishes in tens of milliseconds; this is orders of magnitude above that. Bounding it at
    /// all is the point -- an unbounded wait turns any lost wakeup into a silent guest that only
    /// the sweep's watchdog ends, with no backtrace and no idea which thread stopped.
    const BATCH_LIMIT: Duration = Duration::from_secs(30);

    #[kernel_test]
    fn test_mutex() {
        const ITERS: usize = 50;
        const INNER_ITER: usize = 80;
        for _ in 0..ITERS {
            log!(".");
            for nr_threads in
                (1..max(8, NR_CPUS.load(core::sync::atomic::Ordering::SeqCst) * 2)).step_by(2)
            {
                let lock = Arc::new(Mutex::new(0));
                let mut locks = Vec::new();
                locks.extend((0..nr_threads).into_iter().map(|_| lock.clone()));
                let handles: Vec<_> = locks
                    .into_iter()
                    .map(|lock| {
                        run_closure_in_new_thread(Priority::USER, move || {
                            for _ in 0..INNER_ITER {
                                *lock.lock() += 1;
                                // Outside the guard deliberately. Sleeping while holding it makes
                                // every other worker -- and the parent's wait below -- depend on
                                // the timeout firing, so a lost timer wake reads here as a mutex
                                // hang. Between acquisitions it still spreads arrival order out.
                                if quick_random() % 20 == 0 {
                                    let _ = sys_thread_sync(
                                        &mut [],
                                        Some(&mut Duration::from_millis(1)),
                                    );
                                }
                            }
                        })
                    })
                    .collect();

                for (nth, (thread, closure)) in handles.into_iter().enumerate() {
                    if closure.wait_timeout(BATCH_LIMIT).is_some() {
                        continue;
                    }
                    // Sampled and copied out before panicking: the queue spinlock must not be held
                    // across the unwind, and these are the fields that separate the failure modes
                    // -- an owner that is gone with `handoff` still set is a lost handoff, waiters
                    // queued behind a live owner is a lock that is merely held too long.
                    let (waiters, owned, handoff, owner, owner_state) = {
                        let queue = lock.queue.lock();
                        let owner = queue.owner.as_ref();
                        (
                            queue.queue.iter().count(),
                            queue.owned,
                            queue.handoff,
                            owner.map(|o| o.id()),
                            owner.map(|o| o.get_state()),
                        )
                    };
                    panic!(
                        "test_mutex: worker {} of {} (thread {}, state {:?}) did not finish in \
                         {:?}; mutex owned {} handoff {} owner {:?} ({:?}) waiters {}",
                        nth,
                        nr_threads,
                        thread.id(),
                        thread.get_state(),
                        BATCH_LIMIT,
                        owned,
                        handoff,
                        owner,
                        owner_state,
                        waiters,
                    );
                }
                let inner = lock.lock();
                let val = *inner;
                drop(inner);
                assert_eq!(val, nr_threads * INNER_ITER);
            }
        }
    }

    /// Block until `n` threads are queued on `lock`'s wait list, so waiter arrival order is
    /// established by observation rather than by guessing at sleep durations.
    fn wait_for_waiters<T>(lock: &Mutex<T>, n: usize) {
        for _ in 0..10000 {
            let queued = {
                let queue = lock.queue.lock();
                queue.queue.iter().count()
            };
            if queued >= n {
                return;
            }
            let _ = sys_thread_sync(&mut [], Some(&mut Duration::from_millis(1)));
        }
        panic!("waiters never blocked on the mutex");
    }

    #[kernel_test]
    fn test_mutex_priority_handoff() {
        let lock = Arc::new(Mutex::new(0u32));
        let order: Arc<Spinlock<Vec<u32>>> = Arc::new(Spinlock::new(Vec::new()));

        let guard = lock.lock();

        // Queue the low-priority waiter first, so FIFO order and priority order disagree.
        let bg = {
            let (lock, order) = (lock.clone(), order.clone());
            run_closure_in_new_thread(Priority::BACKGROUND, move || {
                let _g = lock.lock();
                order.lock().push(1u32);
            })
        };
        wait_for_waiters(&lock, 1);

        let rt = {
            let (lock, order) = (lock.clone(), order.clone());
            run_closure_in_new_thread(Priority::REALTIME, move || {
                let _g = lock.lock();
                order.lock().push(2u32);
            })
        };
        wait_for_waiters(&lock, 2);

        drop(guard);
        bg.1.wait();
        rt.1.wait();

        let order = order.lock();
        assert_eq!(
            &order[..],
            &[2u32, 1][..],
            "release() must hand off to the realtime waiter before the background one"
        );
    }
}
