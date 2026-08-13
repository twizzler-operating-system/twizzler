use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    panic::Location,
    sync::atomic::{AtomicPtr, AtomicU64, Ordering},
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

#[repr(align(64))]
struct AlignedAtomicU64(AtomicU64);
pub struct GenericSpinlock<T, Relax: RelaxStrategy> {
    next_ticket: AlignedAtomicU64,
    current: AlignedAtomicU64,
    cell: UnsafeCell<T>,
    locked_from: AtomicPtr<Location<'static>>,
    _pd: PhantomData<Relax>,
}

pub type ReschedulingSpinlock<T> = GenericSpinlock<T, Reschedule>;
pub type Spinlock<T> = GenericSpinlock<T, SpinLoop>;

impl<T, Relax: RelaxStrategy> GenericSpinlock<T, Relax> {
    pub const fn new(data: T) -> Self {
        Self {
            next_ticket: AlignedAtomicU64(AtomicU64::new(0)),
            current: AlignedAtomicU64(AtomicU64::new(0)),
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
        let ticket = self.next_ticket.0.fetch_add(1, Ordering::Relaxed);
        let mut iters = 0;
        spin_wait_until(
            || {
                if self.current.0.load(Ordering::Acquire) != ticket {
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
                    let locked_from = unsafe { self.locked_from.load(Ordering::SeqCst).as_ref() };
                    emerglogln!(
                        "spinlock long pause: {}, locked at {:?}",
                        caller,
                        locked_from
                    );
                }
                Relax::relax(iters);
            },
        );
        self.locked_from
            .store(caller as *const _ as *mut _, Ordering::SeqCst);
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
        let next = self.current.0.load(Ordering::Relaxed) + 1;
        self.current.0.store(next, Ordering::Release);
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
