use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    panic::Location,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::processor::{
    sched::{schedule, SchedFlags},
    spin_wait_until,
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
    #[inline]
    fn relax(_iters: usize) {}
}

#[repr(align(64))]
struct AlignedAtomicU64(AtomicU64);
pub struct GenericSpinlock<T, Relax: RelaxStrategy> {
    next_ticket: AlignedAtomicU64,
    current: AlignedAtomicU64,
    cell: UnsafeCell<T>,
    locked_from: UnsafeCell<Option<Location<'static>>>,
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
            locked_from: UnsafeCell::new(None),
            _pd: PhantomData,
        }
    }

    #[track_caller]
    pub fn lock(&self) -> LockGuard<'_, T, Relax> {
        /* TODO: do we need to set thread critical for this? */
        let interrupt_state = crate::interrupt::disable();
        let ticket = self.next_ticket.0.fetch_add(1, Ordering::Relaxed);
        let mut iters = 0;
        let caller = core::panic::Location::caller().clone();
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
                    emerglogln!("spinlock long pause: {}, locked at {:?}", caller, unsafe {
                        self.locked_from.get().as_ref().unwrap()
                    });
                }
                Relax::relax(iters);
            },
        );
        unsafe { *self.locked_from.get().as_mut().unwrap() = Some(caller) };
        LockGuard {
            lock: self,
            interrupt_state,
            dont_unlock_on_drop: false,
        }
    }

    fn release(&self) {
        let next = self.current.0.load(Ordering::Relaxed) + 1;
        unsafe { *self.locked_from.get().as_mut().unwrap() = None };
        self.current.0.store(next, Ordering::Release);
    }
}

pub struct LockGuard<'a, T, Relax: RelaxStrategy> {
    lock: &'a GenericSpinlock<T, Relax>,
    interrupt_state: bool,
    dont_unlock_on_drop: bool,
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
            self.lock.release();
            crate::interrupt::set(self.interrupt_state);
        }
    }
}

impl<T, Relax: RelaxStrategy> LockGuard<'_, T, Relax> {
    pub fn get_lock(&self) -> &GenericSpinlock<T, Relax> {
        self.lock
    }

    pub unsafe fn force_unlock(&mut self) {
        self.dont_unlock_on_drop = true;
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
