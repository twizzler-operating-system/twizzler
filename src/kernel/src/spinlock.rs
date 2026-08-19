use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    panic::Location,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
};

use crate::{
    processor::{
        sched::{SchedFlags, schedule},
        spin_wait_until,
    },
    thread::locktrack,
};

pub trait RelaxStrategy {
    fn relax(iters: usize);
}

pub struct Reschedule {}
impl RelaxStrategy for Reschedule {
    #[inline]
    fn relax(iters: usize) {
        if iters > 100 {
            schedule(SchedFlags::YIELD | SchedFlags::PREEMPT | SchedFlags::REINSERT);
        }
    }
}
pub struct SpinLoop {}
impl RelaxStrategy for SpinLoop {
    // Empty, and called once per iteration of the spin loop -- at opt-level 0 that is a call
    // instruction per iteration for a function that does nothing.
    #[inline(always)]
    fn relax(_iters: usize) {}
}

/// Both ticket counters on one line: an uncontended cross-core acquire then pays a single line
/// transfer where the old one-aligned-line-per-counter layout cost two. The split existed as
/// armor for the contended case -- each arrival's `fetch_add` on `next` invalidates every
/// spinner's cached read of `current` when they share a line -- and that cost is deliberately
/// accepted: bench_contended_spinlock prices it, bench_pingpong_spinlock prices the win, and
/// kernel spinlocks measure ~4-5% contended (spinpack A/B). u32 tickets suffice because the
/// spin is an equality test, so wrap is harmless short of 2^32 concurrent waiters.
#[repr(align(64))]
struct Tickets {
    next: AtomicU32,
    current: AtomicU32,
}

pub struct GenericSpinlock<T, Relax: RelaxStrategy> {
    tickets: Tickets,
    cell: UnsafeCell<T>,
    locked_from: AtomicPtr<Location<'static>>,
    _pd: PhantomData<Relax>,
}

pub type ReschedulingSpinlock<T> = GenericSpinlock<T, Reschedule>;
pub type Spinlock<T> = GenericSpinlock<T, SpinLoop>;

impl<T, Relax: RelaxStrategy> GenericSpinlock<T, Relax> {
    pub const fn new(data: T) -> Self {
        Self {
            tickets: Tickets {
                next: AtomicU32::new(0),
                current: AtomicU32::new(0),
            },
            cell: UnsafeCell::new(data),
            locked_from: AtomicPtr::new(core::ptr::null_mut()),
            _pd: PhantomData,
        }
    }

    #[track_caller]
    pub fn lock(&self) -> LockGuard<'_, T, Relax> {
        /* TODO: do we need to set thread critical for this? */
        let interrupt_state = crate::interrupt::disable();
        let caller = core::panic::Location::caller();
        // Resolved once: intent, record and release must all land on the same tracker, and the
        // current thread is not stable across a context switch.
        let tracker = locktrack::current_tracker();
        let (intent_thread, intent_cpu) = if locktrack::enabled() {
            (locktrack::diag::this_thread(), locktrack::diag::this_cpu())
        } else {
            (u64::MAX, u32::MAX)
        };
        if let Some(tracker) = tracker {
            locktrack::with_tracker(tracker, |lt| lt.intend_to_lock_spinlock(caller));
        }
        let ticket = self.tickets.next.fetch_add(1, Ordering::Relaxed);
        let mut iters = 0;
        spin_wait_until(
            || {
                if self.tickets.current.load(Ordering::Acquire) != ticket {
                    None
                } else {
                    Some(())
                }
            },
            || {
                iters += 1;
                if iters == 10000 {
                    //emerglogln!("spinlock pause: {}", caller);
                }
                if iters == 100000 {
                    // Valid while held: `release` clears this, so a site named here is the actual
                    // holder rather than whoever acquired last. `None` means the holder had not
                    // recorded itself yet, not that the lock is free.
                    match unsafe { self.locked_from.load(Ordering::Relaxed).as_ref() } {
                        Some(held_by) => emerglogln!(
                            "spinlock long pause: {}, held by {}",
                            caller,
                            held_by
                        ),
                        None => emerglogln!(
                            "spinlock long pause: {}, holder not yet recorded",
                            caller
                        ),
                    }
                }
                Relax::relax(iters);
            },
        );
        // Relaxed: this is read only by the stuck-lock report above, which is already reading a
        // value that may be stale by the time it prints. `SeqCst` made it an `xchg` -- a locked
        // RMW on a third line of this lock, on every acquisition, for a diagnostic.
        self.locked_from
            .store(caller as *const _ as *mut _, Ordering::Relaxed);
        // DIAG: interrupts are off for all of lock(), so neither should change across the spin.
        if locktrack::enabled() {
            let record_thread = locktrack::diag::this_thread();
            let record_cpu = locktrack::diag::this_cpu();
            if (record_thread != intent_thread || record_cpu != intent_cpu)
                && locktrack::diag::INTENT_RECORD_CROSSED.hit()
            {
                emerglogln!(
                    "locktrack: spinlock {} intent on thread {} cpu {}, record on thread {} cpu {} (ints {})",
                    caller,
                    intent_thread,
                    intent_cpu,
                    record_thread,
                    record_cpu,
                    if crate::interrupt::get() { "on" } else { "off" },
                );
            }
        }
        let tracker_index =
            tracker.and_then(|t| locktrack::with_tracker(t, |lt| lt.record_spinlock_lock()));
        LockGuard {
            lock: self,
            interrupt_state,
            dont_unlock_on_drop: false,
            locker: core::panic::Location::caller(),
            tracker,
            tracker_index,
            locked_thread: intent_thread,
        }
    }

    fn release(&self) {
        // Clear the holder record *before* handing the lock on, so that it is never both stale and
        // non-null. Ordered this way deliberately: clearing after the ticket store would race the
        // next holder's own write and could wipe it, turning a correct attribution into `None`.
        // The remaining windows -- between this clear and the ticket store, and between the next
        // acquisition and its store below -- report `None`, i.e. "unknown", never a wrong site.
        //
        // Without this, `locked_from` was a record of the last thread to *acquire*, with no
        // lifetime bound: the site it named had usually already released, and on a hot lock the
        // most frequent acquirer named itself in every report regardless of who held it. Three
        // waiters all reporting one innocent site is what that looked like in practice.
        self.locked_from
            .store(core::ptr::null_mut(), Ordering::Relaxed);
        // wrapping_add is load-bearing at u32: a plain `+` is an overflow panic in debug builds
        // at the boundary, once per 2^32 acquisitions -- surfacing rarely and unattributably.
        let next = self.tickets.current.load(Ordering::Relaxed).wrapping_add(1);
        self.tickets.current.store(next, Ordering::Release);
    }
}

#[must_use = "a dropped guard releases immediately; bind it to a variable"]
pub struct LockGuard<'a, T, Relax: RelaxStrategy> {
    lock: &'a GenericSpinlock<T, Relax>,
    interrupt_state: bool,
    dont_unlock_on_drop: bool,
    pub locker: &'static core::panic::Location<'static>,
    /// Captured at lock time, so `tracker_index` always indexes the tracker it came from.
    tracker: Option<&'static locktrack::LockTracker>,
    tracker_index: Option<usize>,
    /// DIAG: thread current at acquisition, for reporting only.
    locked_thread: u64,
}

pub type SpinLockGuard<'a, T> = LockGuard<'a, T, SpinLoop>;

impl<T, Relax: RelaxStrategy> core::ops::Deref for LockGuard<'_, T, Relax> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.cell.get() }
    }
}

impl<T, Relax: RelaxStrategy> core::ops::DerefMut for LockGuard<'_, T, Relax> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.cell.get() }
    }
}

impl<T, Relax: RelaxStrategy> Drop for LockGuard<'_, T, Relax> {
    fn drop(&mut self) {
        if !self.dont_unlock_on_drop {
            self.check_thread_crossing();
            if let (Some(tracker), Some(index)) = (self.tracker, self.tracker_index) {
                locktrack::with_tracker(tracker, |lt| lt.record_spinlock_unlock(index));
            }
            self.lock.release();
            crate::interrupt::set(self.interrupt_state);
        }
    }
}

impl<T, Relax: RelaxStrategy> LockGuard<'_, T, Relax> {
    /// DIAG: a guard released while a different thread is current than at acquisition.
    fn check_thread_crossing(&self) {
        if !locktrack::enabled() {
            return;
        }
        let now = locktrack::diag::this_thread();
        if now != self.locked_thread && locktrack::diag::SPINLOCK_GUARD_CROSSED.hit() {
            emerglogln!(
                "locktrack: spinlock {} locked by thread {}, released by thread {} (cpu {})",
                self.locker,
                self.locked_thread,
                now,
                locktrack::diag::this_cpu(),
            );
        }
    }

    pub fn get_lock(&self) -> &GenericSpinlock<T, Relax> {
        self.lock
    }

    pub unsafe fn force_unlock(&mut self) {
        self.dont_unlock_on_drop = true;

        if let (Some(tracker), Some(index)) = (self.tracker, self.tracker_index) {
            locktrack::with_tracker(tracker, |lt| lt.record_spinlock_unlock(index));
        }
        self.lock.release();
    }

    pub unsafe fn force_relock(self) -> Self {
        let mut new_guard = self.lock.lock();
        new_guard.interrupt_state = self.interrupt_state;
        new_guard
    }

    pub fn int_state(&self) -> bool {
        self.interrupt_state
    }
}

unsafe impl<T, Relax: RelaxStrategy> Send for GenericSpinlock<T, Relax> where T: Send {}
unsafe impl<T, Relax: RelaxStrategy> Sync for GenericSpinlock<T, Relax> where T: Send {}
unsafe impl<T, Relax: RelaxStrategy> Send for LockGuard<'_, T, Relax> where T: Send {}
unsafe impl<T, Relax: RelaxStrategy> Sync for LockGuard<'_, T, Relax> where T: Send + Sync {}

mod test {
    use alloc::{sync::Arc, vec::Vec};
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use twizzler_kernel_macros::kernel_test;

    use super::Spinlock;
    use crate::{
        instant::Instant,
        processor::mp::NR_CPUS,
        syscall::sync::sys_thread_sync,
        thread::{entry::run_closure_in_new_thread, locktrack, priority::Priority},
    };

    const WAIT_LIMIT: Duration = Duration::from_secs(30);

    /// Cost of an uncontended lock/unlock pair; the mirror of `mutex::bench_uncontended_mutex`,
    /// same batching and reporting so the two are directly comparable in one transcript.
    #[kernel_test]
    fn bench_uncontended_spinlock() {
        const BATCH: usize = 100;
        const BATCHES: usize = 1000;
        let lock = Spinlock::new(0usize);
        for _ in 0..BATCH {
            *lock.lock() += 1;
        }
        let mut samples: Vec<u64> = Vec::with_capacity(BATCHES);
        for _ in 0..BATCHES {
            let start = Instant::now();
            for _ in 0..BATCH {
                *lock.lock() += 1;
            }
            samples.push((Instant::now() - start).as_nanos() as u64);
        }
        samples.sort_unstable();
        let mean = samples.iter().sum::<u64>() / BATCHES as u64;
        emerglogln!(
            "bench_uncontended_spinlock: best {} median {} mean {} ns/op ({} batches of {})",
            samples[0] / BATCH as u64,
            samples[BATCHES / 2] / BATCH as u64,
            mean / BATCH as u64,
            BATCHES,
            BATCH,
        );
        let val = *lock.lock();
        assert_eq!(val, BATCH * (BATCHES + 1));
    }

    /// Spin until `turn` reads `want`, bounded by wall clock; see the identical helper in
    /// `mutex.rs` tests for the full reasoning (REALTIME driver + USER peer means a
    /// co-scheduled peer starves and an unbounded spin wedges the sweep silently; deadline
    /// rather than iteration count so slow profiles cannot fire spuriously; clock read once per
    /// 4096 spins).
    fn spin_turn(turn: &AtomicUsize, want: usize) {
        let mut spins = 0usize;
        let mut start: Option<Instant> = None;
        while turn.load(Ordering::Acquire) != want {
            core::hint::spin_loop();
            spins += 1;
            if spins % 4096 == 0 {
                let now = Instant::now();
                let s = *start.get_or_insert(now);
                if now - s > Duration::from_secs(5) {
                    panic!(
                        "pingpong turn starved (co-scheduled peer?): {} spins in {:?}",
                        spins,
                        now - s
                    );
                }
            }
        }
    }

    /// Cross-core uncontended cost: strict alternation, so every acquire finds the lock free but
    /// its lines last written by the other core. This is the bench that prices the ticket
    /// counters' cache-line layout. Placement is verified, not requested (see the mutex twin).
    #[kernel_test]
    fn bench_pingpong_spinlock() {
        const OPS: usize = 20_000;
        if NR_CPUS.load(Ordering::SeqCst) < 2 {
            emerglogln!("bench_pingpong_spinlock: skipped (1 cpu)");
            return;
        }
        let lock = Arc::new(Spinlock::new(0usize));
        let turn = Arc::new(AtomicUsize::new(0));
        let peer_cpu = Arc::new(AtomicUsize::new(usize::MAX));
        let peer = {
            let (lock, turn, peer_cpu) = (lock.clone(), turn.clone(), peer_cpu.clone());
            run_closure_in_new_thread(Priority::USER, move || {
                for _ in 0..OPS {
                    spin_turn(&turn, 1);
                    *lock.lock() += 1;
                    peer_cpu.store(locktrack::diag::this_cpu() as usize, Ordering::Relaxed);
                    turn.store(0, Ordering::Release);
                }
            })
        };
        let mut same_cpu = 0usize;
        let start = Instant::now();
        for _ in 0..OPS {
            spin_turn(&turn, 0);
            *lock.lock() += 1;
            if locktrack::diag::this_cpu() as usize == peer_cpu.load(Ordering::Relaxed) {
                same_cpu += 1;
            }
            turn.store(1, Ordering::Release);
        }
        if peer.1.wait_timeout(WAIT_LIMIT).is_none() {
            panic!("pingpong peer did not finish in {:?}", WAIT_LIMIT);
        }
        let total = Instant::now() - start;
        emerglogln!(
            "bench_pingpong_spinlock: {} ns/op over {} alternating ops ({} co-scheduled turns)",
            total.as_nanos() as u64 / (2 * OPS) as u64,
            2 * OPS,
            same_cpu,
        );
        let val = *lock.lock();
        assert_eq!(val, 2 * OPS);
    }

    /// Contended cost at two shapes: ~2 waiters, where each arrival's ticket RMW invalidating
    /// the spinners' line costs the most relative to the work protected, and all cpus. Zero and
    /// all-cpus alone is bimodal; real kernel locks live in between. Workers never exceed cpu
    /// count: a waiter spins with interrupts off, so an oversubscribed spinner would wedge the
    /// cpu its holder needs.
    #[kernel_test]
    fn bench_contended_spinlock() {
        let ncpus = NR_CPUS.load(Ordering::SeqCst);
        if ncpus < 2 {
            emerglogln!("bench_contended_spinlock: skipped (1 cpu)");
            return;
        }
        let mut shapes: Vec<usize> = alloc::vec![3.min(ncpus), ncpus];
        shapes.dedup();
        for nthreads in shapes {
            const TOTAL_OPS: usize = 300_000;
            let per_thread = TOTAL_OPS / nthreads;
            let lock = Arc::new(Spinlock::new(0usize));
            let ready = Arc::new(AtomicUsize::new(0));
            let go = Arc::new(AtomicUsize::new(0));
            let handles: Vec<_> = (0..nthreads)
                .map(|_| {
                    let (lock, ready, go) = (lock.clone(), ready.clone(), go.clone());
                    run_closure_in_new_thread(Priority::USER, move || {
                        ready.fetch_add(1, Ordering::Release);
                        while go.load(Ordering::Acquire) == 0 {
                            core::hint::spin_loop();
                        }
                        for _ in 0..per_thread {
                            *lock.lock() += 1;
                        }
                    })
                })
                .collect();
            // Sleep-poll rather than spin: the workers' spin on `go` already occupies cpus.
            let mut polls = 0;
            while ready.load(Ordering::Acquire) != nthreads {
                let _ = sys_thread_sync(&mut [], Some(&mut Duration::from_millis(1)));
                polls += 1;
                assert!(polls < 30_000, "contended-bench workers never became ready");
            }
            let start = Instant::now();
            go.store(1, Ordering::Release);
            for (_, closure) in handles {
                if closure.wait_timeout(WAIT_LIMIT).is_none() {
                    panic!("contended-bench worker did not finish in {:?}", WAIT_LIMIT);
                }
            }
            let total = Instant::now() - start;
            let ops = per_thread * nthreads;
            emerglogln!(
                "bench_contended_spinlock: {} threads: {} ns/op over {} ops",
                nthreads,
                total.as_nanos() as u64 / ops as u64,
                ops,
            );
            let val = *lock.lock();
            assert_eq!(val, ops);
        }
    }
}
