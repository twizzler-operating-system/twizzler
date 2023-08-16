//! Very simple and unsafe Mutex for internal locking needs. DO NOT USE, USE THE RUST STANDARD
//! LIBRARY MUTEX INSTEAD.

use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering},
};

/*
KANI_TODO
*/

use crate::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};

/// Simple mutex, supporting sleeping and wakeup. Does no attempt at handling priority or fairness.
pub(crate) struct MutexImp {
    lock: AtomicU64,
}

unsafe impl Send for MutexImp {}

impl MutexImp {
    /// Construct a new mutex.
    #[allow(dead_code)]
    pub const fn new() -> MutexImp {
        MutexImp {
            lock: AtomicU64::new(0),
        }
    }

    #[allow(dead_code)]
    pub fn is_locked(&self) -> bool {
        self.lock.load(Ordering::SeqCst) != 0
    }

    #[inline]
    /// Lock a mutex, which can be unlocked by calling [Mutex::unlock].
    /// # Safety
    /// The caller must ensure that they are not recursively locking, that they unlock the
    /// mutex correctly, and that any data protected by the mutex is only accessed with the mutex
    /// locked.
    ///
    /// Note, this is why you should use the standard library mutex, which enforces all of these
    /// things.
    #[allow(dead_code)]
    pub unsafe fn lock(&self) {
        for _ in 0..100 {
            let result = self
                .lock
                .compare_exchange_weak(0, 1, Ordering::SeqCst, Ordering::SeqCst);
            if result.is_ok() {
                return;
            }
            core::hint::spin_loop();
        }
        let _ = self
            .lock
            .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst);
        let sleep = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.lock),
            2,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        loop {
            let state = self.lock.swap(2, Ordering::SeqCst);
            if state == 0 {
                break;
            }
            let _ = sys_thread_sync(&mut [sleep], None);
        }
    }

    #[inline]
    /// Unlock a mutex locked with [Mutex::lock].
    /// # Safety
    /// Must be the current owner of the locked mutex and must make sure to unlock properly.
    #[allow(dead_code)]
    pub unsafe fn unlock(&self) {
        if self.lock.swap(0, Ordering::SeqCst) == 1 {
            return;
        }
        for _ in 0..200 {
            if self.lock.load(Ordering::SeqCst) > 0
                && self
                    .lock
                    .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
                    != Err(0)
            {
                return;
            }
            core::hint::spin_loop();
        }
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(&self.lock),
            1,
        ));
        let _ = sys_thread_sync(&mut [wake], None);
    }

    #[inline]
    /// Similar to [Mutex::lock], but if we can't immediately grab the lock, don't and return false.
    /// Return true if we got the lock.
    /// # Safety
    /// Same safety concerns as [Mutex::lock], but now you have to check to see if the lock happened
    /// or not.
    #[allow(dead_code)]
    pub unsafe fn try_lock(&self) -> bool {
        self.lock
            .compare_exchange_weak(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

pub(crate) struct Mutex<T> {
    imp: MutexImp,
    data: UnsafeCell<T>,
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self {
            imp: MutexImp::new(),
            data: Default::default(),
        }
    }
}

impl<T> Mutex<T> {
    #[allow(dead_code)]
    pub const fn new(data: T) -> Self {
        Self {
            imp: MutexImp::new(),
            data: UnsafeCell::new(data),
        }
    }

    #[allow(dead_code)]
    pub fn lock(&self) -> LockGuard<'_, T> {
        unsafe {
            self.imp.lock();
        }
        LockGuard { lock: self }
    }

    #[allow(dead_code)]
    pub fn try_lock(&self) -> Option<LockGuard<'_, T>> {
        unsafe {
            if !self.imp.try_lock() {
                return None;
            }
        }
        Some(LockGuard { lock: self })
    }
}

unsafe impl<T> Send for Mutex<T> where T: Send {}
unsafe impl<T> Sync for Mutex<T> where T: Send {}
unsafe impl<T> Send for LockGuard<'_, T> where T: Send {}
unsafe impl<T> Sync for LockGuard<'_, T> where T: Send + Sync {}

pub(crate) struct LockGuard<'a, T> {
    lock: &'a Mutex<T>,
}

impl<T> core::ops::Deref for LockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> core::ops::DerefMut for LockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for LockGuard<'_, T> {
    fn drop(&mut self) {
        unsafe { self.lock.imp.unlock() };
    }
}
