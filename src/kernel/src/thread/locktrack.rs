use alloc::{boxed::Box, sync::Arc};
use core::{cell::UnsafeCell, sync::atomic::AtomicBool};

use crate::{
    arch::processor::spin_wait_iteration,
    instant::Instant,
    spinlock::Spinlock,
    thread::{Thread, current_thread_ref},
};

pub struct LockTrackerInner {
    mutexes: heapless::Vec<Option<Lock>, 16>,
    spinlocks: heapless::Vec<Option<Lock>, 16>,
    intended_to_mutexlock: Option<Lock>,
    intended_to_spinlock: Option<Lock>,
    id: u64,
}

pub struct LockTracker {
    inner: Box<UnsafeCell<LockTrackerInner>>,
    lock: AtomicBool,
    id: u64,
}

#[derive(Debug)]
pub struct Lock {
    caller: &'static core::panic::Location<'static>,
    locked: bool,
    time: Instant,
    owner: Option<(u64, &'static core::panic::Location<'static>)>,
}

impl Lock {
    pub fn caller(&self) -> &'static core::panic::Location<'static> {
        self.caller
    }

    pub fn new(caller: &'static core::panic::Location<'static>, time: Instant) -> Self {
        Self {
            caller,
            locked: true,
            time,
            owner: None,
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
        }
    }

    pub unsafe fn inner(&self) -> &mut LockTrackerInner {
        unsafe { &mut *self.inner.get() }
    }

    pub fn lock(&self) -> &mut LockTrackerInner {
        assert!(!crate::interrupt::get());
        let mut iter = 0;
        while self.lock.swap(true, core::sync::atomic::Ordering::Acquire) {
            spin_wait_iteration();
            if iter == 10000 {
                emerglogln!("locktracker lock pause");
                self.unlock();
                crate::panic::backtrace(false, None);
            }
            iter += 1;
        }
        unsafe { &mut *self.inner.get() }
    }

    pub fn try_lock(&self) -> Option<&mut LockTrackerInner> {
        if !self.lock.swap(true, core::sync::atomic::Ordering::Acquire) {
            Some(unsafe { &mut *self.inner.get() })
        } else {
            None
        }
    }

    pub fn unlock(&self) {
        self.lock
            .store(false, core::sync::atomic::Ordering::Release);
    }

    pub fn print_locks(&self) {
        let int = crate::interrupt::disable();
        let inner = unsafe { self.inner() };
        inner.print_locks();
        crate::interrupt::set(int);
    }

    pub fn held_locks(&self) -> usize {
        let int = crate::interrupt::disable();
        let inner = self.lock();
        let count = inner.mutex_count() + inner.spinlock_count();
        self.unlock();
        crate::interrupt::set(int);
        count
    }

    pub fn mutex_count(&self) -> usize {
        let int = crate::interrupt::disable();
        let inner = self.lock();
        let count = inner.mutex_count();
        self.unlock();
        crate::interrupt::set(int);
        count
    }

    pub fn spinlock_count(&self) -> usize {
        let int = crate::interrupt::disable();
        let inner = self.lock();
        let count = inner.spinlock_count();
        self.unlock();
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
            lock.owner = Some((thread_id, from));
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
        assert!(
            self.intended_to_mutexlock.is_none(),
            "intended to lock mutex already set to {:?} (from {})",
            self.intended_to_mutexlock.as_ref(),
            caller
        );
        self.intended_to_mutexlock = Some(Lock::new(caller, time));
    }

    pub fn record_mutex_lock(&mut self) -> Option<usize> {
        let lock = self.intended_to_mutexlock.take().unwrap();
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
        assert!(
            self.intended_to_spinlock.is_none(),
            "intended to lock spinlock already set"
        );
        self.intended_to_spinlock = Some(Lock::new(caller, Instant::zero()));
    }

    pub fn record_spinlock_lock(&mut self) -> Option<usize> {
        let lock = self.intended_to_spinlock.take().unwrap();
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
        assert!(index < self.spinlocks.len(), "invalid spinlock index");
        self.spinlocks.remove(index);
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
        emerglogln!("== LockTracker for thread {}:", self.id);
        if self.mutex_count() > 0 {
            emerglogln!("Mutexes held:");
            for (i, lock) in self.mutexes.iter().enumerate() {
                if let Some(lock) = lock {
                    if lock.is_locked() {
                        emerglogln!("  {}: {} (locked)", i, lock.caller());
                    } else {
                        emerglogln!("  {}: {} (unlocked)", i, lock.caller());
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

        if self.intended_to_mutexlock.is_some() {
            let lock = self.intended_to_mutexlock.as_ref().unwrap();
            if lock.owner.is_some() {
                emerglogln!(
                    "Intend to lock mutex: {} (owned by thread {} at {})",
                    lock.caller(),
                    lock.owner.as_ref().unwrap().0,
                    lock.owner.as_ref().unwrap().1
                );
            } else {
                emerglogln!("Intend to lock mutex: {}", lock.caller());
            }
        }

        if self.intended_to_spinlock.is_some() {
            emerglogln!(
                "Intend to lock spinlock: {}",
                self.intended_to_spinlock.as_ref().unwrap().caller()
            );
        }
    }

    pub fn mutex_wait_time(&self) -> Option<Instant> {
        self.intended_to_mutexlock.as_ref().map(|l| l.time)
    }
}

const DISABLE_LOCK_TRACKING: bool = !cfg!(debug_assertions);

pub fn with_lock_tracker<R: Default>(f: impl FnOnce(&mut LockTrackerInner) -> R) -> R {
    if DISABLE_LOCK_TRACKING {
        return R::default();
    }
    let Some(ct) = current_thread_ref() else {
        return R::default();
    };
    let int = crate::interrupt::disable();
    let inner = ct.lock_tracker.lock();
    let r = f(inner);
    ct.lock_tracker.unlock();
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

pub fn register_lock_tracker(tracker: Arc<LockTracker>) -> Option<usize> {
    let int = crate::interrupt::disable();
    let mut at = ALL_TRACKERS.lock();
    let pos = at.iter().position(|t| t.is_none());
    if let Some(pos) = pos {
        at[pos] = Some(tracker);
        crate::interrupt::set(int);
        return Some(pos);
    }
    let len = at.len();
    let result = at.push(Some(tracker));
    crate::interrupt::set(int);
    result.ok().map(|_| len)
}

pub fn deregister_lock_tracker(index: usize) {
    let int = crate::interrupt::disable();
    let mut at = ALL_TRACKERS.lock();
    if index < at.len() {
        assert!(at[index].as_ref().is_none_or(|x| x.held_locks() == 0));
        at[index] = None;
    }
    crate::interrupt::set(int);
}

pub fn check_timed_out_mutexes() {
    let now = Instant::now();

    let int = crate::interrupt::disable();
    let at = ALL_TRACKERS.lock();
    //emerglogln!("checking {} threads for timed out mutexes", at.len());
    let mut any = false;
    for th in at.iter() {
        let Some(lock_tracker) = th.as_ref() else {
            continue;
        };
        let Some(lt) = lock_tracker.try_lock() else {
            emerglogln!("failed to lock lock tracker for thread {}", lock_tracker.id,);
            continue;
        };
        if lt
            .mutex_wait_time()
            .is_some_and(|t| (now - t).as_millis() > 1000)
        {
            emerglogln!(
                "Thread {} has been waiting on a mutex for more than 1 second",
                lock_tracker.id,
            );
            any = true;
        }
        lock_tracker.unlock();
    }

    if any {
        for th in at.iter() {
            let Some(lock_tracker) = th.as_ref() else {
                continue;
            };
            let Some(lt) = lock_tracker.try_lock() else {
                emerglogln!("failed to lock lock tracker for thread {}", lock_tracker.id,);
                continue;
            };
            lt.print_locks();
            lock_tracker.unlock();
        }
    }

    crate::interrupt::set(int);
}
