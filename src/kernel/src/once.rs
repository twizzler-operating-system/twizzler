use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

use crate::{condvar::CondVar, processor::spin_wait_until, spinlock::Spinlock};

/// One-time lazy initialization where waiters **spin**.
///
/// Use this when any caller may be in a context that cannot sleep: interrupt handlers, early boot
/// before threading, the allocator and memory tracker, the scheduler, or anything reached from
/// inside `Mutex::lock` itself. Because waiters spin without yielding, a waiter that can starve
/// the initializer deadlocks on a uniprocessor -- see [`Once::wait`].
///
/// When every caller can sleep, prefer [`OnceWait`], whose waiters block instead.
pub struct Once<T> {
    status: AtomicU32,
    data: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: Once call_once has been issued, the underlying data structure is made available,
// and we internally manage consistency of the unsafecell and the status.
unsafe impl<T: Send + Sync> Sync for Once<T> {}
unsafe impl<T: Send> Send for Once<T> {}

const INCOMPLETE: u32 = 0;
const RUNNING: u32 = 1;
const COMPLETE: u32 = 2;

impl<T> Once<T> {
    /// Constructs a new Once with uninitialized data, must be initialized with call_once before it
    /// will return any data.
    pub const fn new() -> Self {
        Self {
            status: AtomicU32::new(INCOMPLETE),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
    /// Initialize the data once and only once, returning the data once it is initialized. The given
    /// closure will only execute the first time this function is called, and otherwise will not be
    /// run.
    ///
    /// If multiple calls to call_once race, only one of them will run and initialize the data, the
    /// others will block.
    pub fn call_once<F: FnOnce() -> T>(&self, f: F) -> &T {
        let status = self.status.load(Ordering::SeqCst);
        if status == INCOMPLETE {
            match self.status.compare_exchange(
                INCOMPLETE,
                RUNNING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    // We will initialize this Once.
                    // SAFETY: We are the only ones who can access the UnsafeCell, here, since we
                    // succeeded the cmpxchg above.
                    unsafe {
                        (*self.data.get()).as_mut_ptr().write(f());
                    }
                    self.status.store(COMPLETE, Ordering::SeqCst);
                }
                Err(_) => {
                    return self.wait();
                }
            }
        } else if status == RUNNING {
            return self.wait();
        }
        // SAFETY: Data will never change, since the status is COMPLETE, and the data is
        // initialized, for the same reason.
        return unsafe { self.force_get() };
    }

    unsafe fn force_get(&self) -> &T {
        unsafe { &*(*self.data.get()).as_ptr() }
    }

    /// If the data is not ready, then return None, or return Some if the data is ready. If this
    /// races with a call to call_once, the function will either return None or wait until the data
    /// is ready and return Some.
    pub fn poll(&self) -> Option<&T> {
        let status = spin_wait_until(
            || match self.status.load(Ordering::SeqCst) {
                COMPLETE => Some(COMPLETE),
                INCOMPLETE => Some(INCOMPLETE),
                _ => None,
            },
            || {},
        );

        if status == COMPLETE {
            // SAFETY: If status is complete, the data is ready.
            Some(unsafe { self.force_get() })
        } else {
            None
        }
    }

    /// Whether the data is initialized, without ever waiting.
    ///
    /// Unlike [`Self::poll`], this never spins: it returns false while another thread is still
    /// running the initializer. Callers that must not block on someone else's initialization --
    /// notably anything running on the idle thread -- have to use this. See the deadlock note on
    /// [`Self::wait`].
    pub fn is_complete(&self) -> bool {
        self.status.load(Ordering::SeqCst) == COMPLETE
    }

    /// Wait until the data is ready (someone calls call_once).
    ///
    /// This spins without yielding, so the waiter must not be able to starve the initializer. On a
    /// uniprocessor that means never waiting on an initializer running at a lower priority: the
    /// waiter stays runnable forever and the initializer is never scheduled again. Use
    /// [`Self::is_complete`] from contexts that cannot make that guarantee, or [`OnceWait`] when
    /// every caller can sleep -- see the note on this type.
    pub fn wait(&self) -> &T {
        spin_wait_until(|| self.poll(), || {})
    }
}

impl<T> Drop for Once<T> {
    fn drop(&mut self) {
        // We don't have to check for running here, since we have &mut access to self.
        if self.status.load(Ordering::SeqCst) == COMPLETE {
            unsafe {
                core::ptr::drop_in_place((*self.data.get()).as_mut_ptr());
            }
        }
    }
}

/// One-time lazy initialization where waiters **block** on a condvar.
///
/// Prefer this to [`Once`] whenever every caller can sleep, which in practice means anything
/// guarding a sleeping `Mutex`: a blocked waiter is not runnable, so it cannot starve the
/// initializer no matter their relative priorities or how many cpus there are. That is the failure
/// [`Once`] is subject to.
///
/// Not usable from interrupt context, early boot, the allocator, or the scheduler -- those need
/// [`Once`].
pub struct OnceWait<T> {
    ready: AtomicBool,
    lock: Spinlock<bool>,
    cv: CondVar,
    data: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: Once call_once has been issued, the underlying data structure is made available,
// and we internally manage consistency of the unsafecell and the status.
unsafe impl<T: Send + Sync> Sync for OnceWait<T> {}
unsafe impl<T: Send> Send for OnceWait<T> {}

impl<T> OnceWait<T> {
    /// Constructs a new Once with uninitialized data, must be initialized with call_once before it
    /// will return any data.
    pub const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            lock: Spinlock::new(false),
            cv: CondVar::new(),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
    /// Initialize the data once and only once, returning the data once it is initialized. The given
    /// closure will only execute the first time this function is called, and otherwise will not be
    /// run.
    ///
    /// If multiple calls to call_once race, only one of them will run and initialize the data, the
    /// others will block and wait (be descheduled).
    pub fn call_once<F: FnOnce() -> T>(&self, f: F) -> &T {
        let ready = self.ready.load(Ordering::SeqCst);
        if !ready {
            let mut guard = self.lock.lock();
            if *guard {
                drop(guard);
                return self.wait();
            }
            *guard = true;
            // Release before running the initializer. It may allocate, and allocation can sleep
            // (alloc_frame with WAIT_OK waits on a condvar) -- sleeping while holding a spinlock
            // is the hazard mutex.rs warns about. The in-progress flag keeps other callers out.
            drop(guard);
            // SAFETY: We are the only ones who can access the UnsafeCell, here, since we
            // succeeded in locking with *guard == false and set it before releasing.
            unsafe {
                (*self.data.get()).as_mut_ptr().write(f());
            }
            // Publish under the lock: wait() checks poll() while holding it and then sleeps, so
            // storing outside would open a lost-wakeup window between that check and the sleep.
            let guard = self.lock.lock();
            self.ready.store(true, Ordering::SeqCst);
            drop(guard);
            self.cv.signal();
        }
        // SAFETY: Data will never change, since the status is COMPLETE, and the data is
        // initialized, for the same reason.
        return unsafe { self.force_get() };
    }

    unsafe fn force_get(&self) -> &T {
        unsafe { &*(*self.data.get()).as_ptr() }
    }

    /// Whether the data is initialized, without waiting.
    pub fn is_complete(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// If the data is not ready, then return None, or return Some if the data is ready.
    pub fn poll(&self) -> Option<&T> {
        if self.ready.load(Ordering::SeqCst) {
            // SAFETY: data is ready.
            Some(unsafe { self.force_get() })
        } else {
            None
        }
    }

    /// Wait until the data is ready (someone calls call_once).
    pub fn wait(&self) -> &T {
        spin_wait_until(
            || self.poll(),
            || {
                let guard = self.lock.lock();
                if self.poll().is_none() {
                    let _ = self.cv.wait(guard);
                }
            },
        )
    }
}

impl<T> Drop for OnceWait<T> {
    fn drop(&mut self) {
        if self.ready.load(Ordering::SeqCst) {
            unsafe {
                core::ptr::drop_in_place((*self.data.get()).as_mut_ptr());
            }
        }
    }
}
