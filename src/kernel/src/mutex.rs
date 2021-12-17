use core::{cell::UnsafeCell, sync::atomic::AtomicU64};

use alloc::collections::VecDeque;

use crate::{
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

pub struct Mutex<T> {
    queue: Spinlock<SleepQueue>,
    cell: UnsafeCell<T>,
}

impl<T> Mutex<T> {
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

    pub fn lock(&self) -> LockGuard<'_, T> {
        let current_thread = current_thread_ref();
        let current_donated_priority = current_thread
            .as_ref()
            .and_then(|t| t.get_donated_priority());

        loop {
            let reinsert = {
                let mut queue = self.queue.lock();
                if !queue.owned {
                    queue.owned = true;
                    if let Some(ref thread) = current_thread {
                        if let Some(ref pri) = queue.pri {
                            thread.donate_priority(pri.clone());
                        }
                    }
                    queue.owner = current_thread;
                    break;
                }

                let mut reinsert = true;
                if let Some(ref thread) = current_thread {
                    if !thread.is_idle_thread() {
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
            thread.set_state(ThreadState::Running);
            sched::schedule_thread(thread);
        } else {
            queue.pri = None;
        }
        queue.owner = None;
        queue.owned = false;
    }
}

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
        self.lock.release();
        if let Some(ref prev) = self.prev_donated_priority {
            if let Some(thread) = current_thread_ref() {
                thread.donate_priority(prev.clone());
            }
        } else if let Some(thread) = current_thread_ref() {
            thread.remove_donated_priority();
        }
    }
}

unsafe impl<T> Send for Mutex<T> where T: Send {}
unsafe impl<T> Sync for Mutex<T> where T: Send {}
unsafe impl<T> Send for LockGuard<'_, T> where T: Send {}
unsafe impl<T> Sync for LockGuard<'_, T> where T: Send + Sync {}
