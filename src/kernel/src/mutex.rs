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

use core::{cell::UnsafeCell, sync::atomic::AtomicU64};

use alloc::collections::VecDeque;

use crate::{
    idcounter::StableId,
    sched,
    spinlock::Spinlock,
    thread::{current_thread_ref, Priority, ThreadRef, ThreadState},
};

#[repr(align(64))]
struct AlignedAtomicU64(AtomicU64);
struct SleepQueue {
    queue: VecDeque<ThreadRef>,
    pri: Option<Priority>,
    owner: Option<ThreadRef>,
    owned: bool,
}

/// A container data structure to manage mutual exclusion.
pub struct Mutex<T> {
    queue: Spinlock<SleepQueue>,
    cell: UnsafeCell<T>,
}

impl<T> Mutex<T> {
    /// Create a new mutex, moving data `T` into it.
    pub fn new(data: T) -> Self {
        Self {
            queue: Spinlock::new(SleepQueue {
                queue: VecDeque::new(),
                pri: None,
                owner: None,
                owned: false,
            }),
            cell: UnsafeCell::new(data),
        }
    }

    /// Lock the mutex and return a lock guard to manage a reference to the managed data. When the
    /// lock guard goes out of scope, the lock will be released.
    pub fn lock(&self) -> LockGuard<'_, T> {
        let current_thread = current_thread_ref();
        let current_donated_priority = current_thread
            .as_ref()
            .and_then(|t| t.get_donated_priority());

        if let Some(ref current_thread) = current_thread {
            /* TODO: maybe try to support critical threads by falling back to a spinloop? */
            assert!(!current_thread.is_critical());
        }

        loop {
            let reinsert = {
                let mut queue = self.queue.lock();
                logln!(
                    "checking queue {} {:p}",
                    current_thread.as_ref().map_or(0, |x| x.id()),
                    self
                );
                if !queue.owned {
                    queue.owned = true;
                    if let Some(ref thread) = current_thread {
                        if let Some(ref pri) = queue.pri {
                            thread.donate_priority(pri.clone());
                        }
                    }
                    logln!(
                        "got it {} {:p}",
                        current_thread.as_ref().map_or(0, |x| x.id()),
                        self
                    );
                    queue.owner = current_thread;
                    break;
                } else {
                    if let Some(ref cur_owner) = queue.owner {
                        if let Some(ref cur_thread) = current_thread {
                            if cur_thread.id() == cur_owner.id() {
                                panic!("this mutex is not re-entrant");
                            }
                        }
                    }
                }

                let mut reinsert = true;
                if let Some(ref thread) = current_thread {
                    if !thread.is_idle_thread() {
                        logln!(
                            "thread {} block on {:p} (owned by {:?})",
                            thread.id(),
                            self,
                            queue.owner.as_ref().map(|x| x.id())
                        );
                        thread.set_state(ThreadState::Blocked);
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

            logln!(
                "sched {} {:p}",
                current_thread.as_ref().map_or(0, |x| x.id()),
                self
            );
            sched::schedule(reinsert);
        }

        LockGuard {
            lock: self,
            prev_donated_priority: current_donated_priority,
        }
    }

    fn release(&self) {
        let mut queue = self.queue.lock();
        if let Some(thread) = queue.queue.pop_front() {
            sched::schedule_thread(thread);
        } else {
            queue.pri = None;
        }
        queue.owner = None;
        queue.owned = false;
    }
}

/// Manages a reference to the data controlled by a mutex.
pub struct LockGuard<'a, T> {
    lock: &'a Mutex<T>,
    prev_donated_priority: Option<Priority>,
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
        if let Some(thread) = current_thread_ref() {
            logln!("dropping mutex {:p} {}", self.lock, thread.id());
        }
        if let Some(ref prev) = self.prev_donated_priority {
            if let Some(thread) = current_thread_ref() {
                thread.donate_priority(prev.clone());
            }
        } else if let Some(thread) = current_thread_ref() {
            thread.remove_donated_priority();
        }
        self.lock.release();
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
                .partial_cmp(&(&*other.cell.get()).id())
        }
    }
}

impl<T> Ord for Mutex<T>
where
    T: StableId,
{
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        unsafe { (&*self.cell.get()).id().cmp(&(&*other.cell.get()).id()) }
    }
}
