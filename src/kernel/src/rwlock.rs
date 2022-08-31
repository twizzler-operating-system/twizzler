use core::{cell::UnsafeCell, ops::{Drop, Deref, DerefMut}};

pub use self::policy::*;

/// An abstraction of inner type managing the [`RwLock`] state
///
/// See also [`ReadPref`] or [`WritePref`] for implementors of this trait.
pub trait RwLockPreference {
    /// Tries to read data, may block self or other threads depending on implementation.
    fn read(&self);
    /// Tries to write data, may block self of other threads depending on implementation.
    fn write(&self);
    /// Sets the lock in the unlocked state, possibly waking up other threads waiting. 
    fn read_unlock(&self);
    /// Sets the lock in the unlocked state, possibly waking up other threads waiting. 
    fn write_unlock(&self);
}

/// A trait with a single method used to initialize different lock implementations.
///
/// This allows different types to implement a new method with [`const`], and thus
/// allows us to use [`RwLock`] in const settings such as with statics.
///
/// See also [`new`].
pub trait RwLockInner {
    /// Returns a new lock in the unlocked state
    fn new() -> Self;
}

/// A reader writer lock allowing multiple readers and only a single writer.
///
/// Depending on implementation readers or writers may starve waiting
/// on each other. Or the implementation may attempt to provide some fairness.
/// The implementation of this lock may or may not take into account (scheduling)
/// priority as well.
///
/// # Examples
/// 
/// We can use `RwLock` in local contexts:
///
/// ```no_run
/// use crate:rwlock::{RwLock, ReadPrefer};
/// // this initialization is valid
/// let data = rwlock::new::<u64, ReadPrefer>(42u64);
/// // or the compiler could infer the type parameters
/// let data: RwLock<u64, ReadPrefer> = rwlock::new(42u64);
/// ```
///
/// Or we can use it in a global context:
///
/// ```no_run
/// use crate::rwlock::{RwLock, ReadPrefer};
///
/// static DATA: RwLock<u64, ReadPrefer> = rwlock::new(42u64);
///
/// fn read_data() -> u64 {
///     let data = DATA.read();
///     let value = *data;
///     return value
/// }
/// ```
///
/// See also [`RwLockPreference`].
pub struct RwLock<T, U> 
where
    U: RwLockPreference + RwLockInner,
{
    inner: U,
    cell: UnsafeCell<T>
}

/// Creates a new read-write lock in the unlocked state.
///
/// The `const_trait_impl` feature has not been stabilized yet
/// and usage of `~const` as a trait bound in struct impl is not allowed.
/// So we have a stand alone method to create different `RwLocks`.
pub const fn new<T,U>(data: T) -> RwLock<T,U>
where
    U: RwLockPreference + ~const RwLockInner
{
    RwLock {
        inner: U::new(),
        cell: UnsafeCell::new(data)
    }
}

impl<T, U> RwLock<T, U>
where
    U: RwLockPreference + RwLockInner,
{
    /// Shared reads across threads. May block.
    pub fn read(&self) -> ReadLockGuard<'_, T, U> {
        self.inner.read();
        ReadLockGuard {
            lock: self
        }
    }

    /// Exlusive write access to the data. May block.
    pub fn write(&self) -> WriteLockGuard<'_, T, U> {
        self.inner.write();
        WriteLockGuard {
            lock: self
        }
    }

    /// Releases reader lock and may wake up waiting writers.
    fn read_unlock(&self) {
        self.inner.read_unlock()
    }

    /// Releases writer lock and may wake up waiting readers or writers.
    fn write_unlock(&self) {
        self.inner.write_unlock()
    }
}

// lock gaurds used to manage references to the shared data.
pub struct ReadLockGuard<'a, T, U>
where
    U: RwLockPreference + RwLockInner,
{
    lock: &'a RwLock<T, U>
}

pub struct WriteLockGuard<'a, T, U>
where
    U: RwLockPreference + RwLockInner,
{
    lock: &'a RwLock<T, U>
}

impl<T, U> Deref for ReadLockGuard<'_, T, U>
where
    U: RwLockPreference + RwLockInner,
{
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.cell.get() }
    }
}

impl<T, U> Deref for WriteLockGuard<'_, T, U>
where
    U: RwLockPreference + RwLockInner,
{
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.cell.get() }
    }
}

impl<T, U> DerefMut for WriteLockGuard<'_, T, U>
where
    U: RwLockPreference + RwLockInner,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.cell.get() }
    }
}

impl<T, U> Drop for ReadLockGuard<'_, T, U>
where
    U: RwLockPreference + RwLockInner,
{
    fn drop(&mut self) {
        self.lock.inner.read_unlock()
    }
}

impl<T, U> Drop for WriteLockGuard<'_, T, U>
where
    U: RwLockPreference + RwLockInner,
{
    fn drop(&mut self) {
        self.lock.inner.write_unlock()
    }
}

// marker traits indicating data type can be shared between threads
unsafe impl<T, U> Send for RwLock<T, U> where T: Send, U: RwLockPreference + RwLockInner {}
unsafe impl<T, U> Sync for RwLock<T, U> where T: Send + Sync, U: RwLockPreference + RwLockInner {}

unsafe impl<T, U> Send for ReadLockGuard<'_, T, U> where T: Send, U: RwLockPreference + RwLockInner {}
unsafe impl<T, U> Sync for ReadLockGuard<'_, T, U> where T: Sync, U: RwLockPreference + RwLockInner {}

unsafe impl<T, U> Send for WriteLockGuard<'_, T, U> where T: Send, U: RwLockPreference + RwLockInner {}
unsafe impl<T, U> Sync for WriteLockGuard<'_, T, U> where T: Sync, U: RwLockPreference + RwLockInner {}

mod policy {    
    use crate::{condvar::CondVar, spinlock::Spinlock};

    use super::{RwLockPreference, RwLockInner};

    /// A read preferring lock that makes writers wait.
    ///
    /// If there are any readers reading the data, writers may
    /// starve if readers keep showing up to read the data. Writers
    /// eventually get to go.
    pub struct ReadPrefer {
        cond: CondVar,
        inner: Spinlock<ReadPreferInner>
    }

    struct ReadPreferInner {
        active_readers: u64,
        active_writer: bool
    }

    impl ReadPreferInner {
        fn others_active(&self) -> bool {
            self.active_readers > 0 || self.active_writer
        }
    }
    
    impl const RwLockInner for ReadPrefer {
        fn new() -> Self {
            ReadPrefer {
                cond: CondVar::new(),
                inner: Spinlock::new(
                    ReadPreferInner {
                        active_readers: 0,
                        active_writer: false
                    }
                )
            }
        }
    }

    impl RwLockPreference for ReadPrefer {
        fn read(&self) {
            let mut lock_state = self.inner.lock();
            // increment number of readers so that others
            // writers know that we are waiting
            lock_state.active_readers += 1;
            // wait for any active modifications on data
            while lock_state.active_writer {
                lock_state = self.cond.wait(lock_state);
            }
            // continue on to read the data
            // lock on the lock_state is released on drop
        }

        fn write(&self) {
            let mut lock_state = self.inner.lock();
            // wait while there are other readers reading the
            // data or other writers modifying it
            while lock_state.others_active() {
                lock_state = self.cond.wait(lock_state);
            }
            // we now hold the lock, time to set the flag
            // to let others know there is a writer modifying the data
            lock_state.active_writer = true
            // continue to write to the data
            // lock on lock_state will unlock when fallen out of scope (Drop)
        }

        fn read_unlock(&self) {
            let mut lock_state = self.inner.lock();
            // remove our presence
            lock_state.active_readers -= 1;
            // notify any others waiting on us
            // if we are the last ones out
            if lock_state.active_readers == 0 {
                self.cond.signal()
            }
            // lock on lock_state will unlock when fallen out of scope (Drop)
        }

        fn write_unlock(&self) {
            let mut lock_state = self.inner.lock();
            // remove our presence
            lock_state.active_writer = false;
            // notify any others waiting on us
            self.cond.signal()
            // lock on lock_state will unlock when fallen out of scope (Drop)
        }
    }

    /// A write preferring lock that favors writers to pass and makes readers wait.
    ///
    /// Readers may starve if writers keep showing up to modify the data, since readers
    /// wait on any writers actively modifying the data or waiting to modify the data.
    /// Writers simply wait on any batch of readers reading the data or other writers.
    pub struct WritePrefer {
        cond: CondVar,
        inner: Spinlock<WritePreferInner>
    }
    
    struct WritePreferInner {
        active_readers: u64,
        pending_writers: u64,
        active_writer: bool
    } 

    impl WritePreferInner {
        fn pending_writers(&self) -> bool {
            self.pending_writers > 0 || self.active_writer
        }
    
        fn others_active(&self) -> bool {
            self.active_readers > 0 || self.active_writer
        }
    }

    impl const RwLockInner for WritePrefer {
        fn new() -> Self {
            WritePrefer {
                cond: CondVar::new(),
                inner: Spinlock::new(
                    WritePreferInner {
                        active_readers: 0,
                        pending_writers: 0,
                        active_writer: false
                    }
                )
            }
        }
    }

    impl RwLockPreference for WritePrefer {
        fn read(&self) {
            // get exclusive access to locks state
            let mut lock_state = self.inner.lock();
            // while there are writers waiting, or active
            while lock_state.pending_writers() {
                // sleep/wait until we can go
                lock_state = self.cond.wait(lock_state);
            }
            // we have woken up, and now hold the lock
            // now we can get shared read access to the data
            lock_state.active_readers += 1;
            // lock on lock_state will unlock when fallen out of scope (Drop)
        }
    
        fn write(&self){
            // get exclusive access to the lock state
            let mut lock_state = self.inner.lock();
            // wait for others ahead of me to finish
            while lock_state.others_active() {
                lock_state = self.cond.wait(lock_state);
            }
            // now that we are awake, we can have exclusive
            // write access to the data
            lock_state.active_writer = true;
            // lock on lock_state will unlock when fallen out of scope (Drop)
        }
    
        fn read_unlock(&self) {
            // get exclusive access to the lock state
            let mut lock_state = self.inner.lock();
            // remove our presence
            lock_state.active_readers -= 1;
            // notify any others waiting on us
            // if we are the last ones out
            if lock_state.active_readers == 0 {
                self.cond.signal()
            }
            // lock on lock_state will unlock when fallen out of scope (Drop)
        }
    
        fn write_unlock(&self) {
            // get exclusive access to the lock state
            let mut lock_state = self.inner.lock();
            // remove our presence
            lock_state.active_writer = false;
            // let everyone else know that we are done
            self.cond.signal()
            // lock on lock_state will unlock when fallen out of scope (Drop)
        }
    }
}
