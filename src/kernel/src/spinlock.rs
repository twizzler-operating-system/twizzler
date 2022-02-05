use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

pub trait RelaxStrategy {
    fn relax(iters: usize);
}

pub struct Reschedule {}
impl RelaxStrategy for Reschedule {
    #[inline]
    fn relax(iters: usize) {
        if iters > 100 {
            crate::sched::schedule(true);
        } else {
            core::hint::spin_loop();
        }
    }
}
pub struct SpinLoop {}
impl RelaxStrategy for SpinLoop {
    #[inline]
    fn relax(_iters: usize) {
        core::hint::spin_loop()
    }
}

#[repr(align(64))]
struct AlignedAtomicU64(AtomicU64);
pub struct GenericSpinlock<T, Relax: RelaxStrategy> {
    next_ticket: AlignedAtomicU64,
    current: AlignedAtomicU64,
    cell: UnsafeCell<T>,
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
            _pd: PhantomData,
        }
    }

    pub fn lock(&self) -> LockGuard<'_, T, Relax> {
        /* TODO: do we need to set thread critical for this? */
        let interrupt_state = crate::interrupt::disable();
        let ticket = self.next_ticket.0.fetch_add(1, Ordering::Relaxed);
        let mut iters = 0;
        while self.current.0.load(Ordering::Acquire) != ticket {
            Relax::relax(iters);
            iters += 1;
        }
        LockGuard {
            lock: self,
            interrupt_state,
            dont_unlock_on_drop: false,
        }
    }

    fn release(&self) {
        let next = self.current.0.load(Ordering::Relaxed) + 1;
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

    pub unsafe fn force_unlock(&self) {
        self.dont_unlock_on_drop = true;
        self.lock.release();
        crate::interrupt::set(self.interrupt_state);
    }

    pub unsafe fn force_relock(self) -> Self {
        self.lock.lock()
    }
}

unsafe impl<T, Relax: RelaxStrategy> Send for GenericSpinlock<T, Relax> where T: Send {}
unsafe impl<T, Relax: RelaxStrategy> Sync for GenericSpinlock<T, Relax> where T: Send {}
unsafe impl<T, Relax: RelaxStrategy> Send for LockGuard<'_, T, Relax> where T: Send {}
unsafe impl<T, Relax: RelaxStrategy> Sync for LockGuard<'_, T, Relax> where T: Send + Sync {}
