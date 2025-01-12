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

use intrusive_collections::{intrusive_adapter, LinkedList};
use twizzler_abi::thread::ExecutionState;

use crate::{
    arch,
    idcounter::StableId,
    sched::{self, schedule_thread},
    spinlock::Spinlock,
    thread::{current_thread_ref, priority::Priority, Thread, ThreadRef},
};

#[repr(align(64))]
struct AlignedAtomicU64(AtomicU64);
struct SleepQueue {
    queue: LinkedList<MutexLinkAdapter>,
    pri: Option<Priority>,
    owner: Option<ThreadRef>,
    owned: bool,
}

intrusive_adapter!(pub MutexLinkAdapter = ThreadRef: Thread { mutex_link: intrusive_collections::linked_list::AtomicLink });

impl Drop for SleepQueue {
    fn drop(&mut self) {
        while let Some(t) = self.queue.pop_front() {
            schedule_thread(t);
        }
    }
}

/// A container data structure to manage mutual exclusion.
pub struct Mutex<T> {
    queue: Spinlock<SleepQueue>,
    cell: UnsafeCell<T>,
}

impl<T> Mutex<T> {
    /// Create a new mutex, moving data `T` into it.
    pub const fn new(data: T) -> Self {
        Self {
            queue: Spinlock::new(SleepQueue {
                queue: LinkedList::new(MutexLinkAdapter::NEW),
                pri: None,
                owner: None,
                owned: false,
            }),
            cell: UnsafeCell::new(data),
        }
    }

    /// Get a mut reference to the contained data. Does not perform locking, but is safe because we
    /// have a mut reference to the mutex itself.
    pub fn get_mut(&mut self) -> &mut T {
        self.cell.get_mut()
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

        let mut istate;
        loop {
            istate = crate::interrupt::disable();
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
                } else if let Some(ref cur_owner) = queue.owner {
                    if let Some(ref cur_thread) = current_thread {
                        if cur_thread.id() == cur_owner.id() {
                            panic!("this mutex is not re-entrant");
                        }
                    }
                }

                let mut reinsert = true;
                if let Some(ref thread) = current_thread {
                    if !thread.is_idle_thread() {
                        thread.set_state(ExecutionState::Sleeping);
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
            arch::processor::spin_wait_iteration();
            core::hint::spin_loop();
            if current_thread.is_some() {
                sched::schedule(reinsert);
            }
            crate::interrupt::set(istate);
        }

        crate::interrupt::set(istate);
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
                .partial_cmp((&*other.cell.get()).id())
        }
    }
}

impl<T> Ord for Mutex<T>
where
    T: StableId,
{
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        unsafe { (&*self.cell.get()).id().cmp((&*other.cell.get()).id()) }
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

mod test {
    use alloc::{sync::Arc, vec::Vec};
    use core::{cmp::max, time::Duration};

    use twizzler_kernel_macros::kernel_test;

    use super::Mutex;
    use crate::{
        processor::NR_CPUS,
        syscall::sync::sys_thread_sync,
        thread::{entry::run_closure_in_new_thread, priority::Priority},
        utils::quick_random,
    };

    #[kernel_test]
    fn test_mutex() {
        const ITERS: usize = 50;
        const INNER_ITER: usize = 80;
        for _ in 0..ITERS {
            log!(".");
            for nr_threads in
                (1..max(8, NR_CPUS.load(core::sync::atomic::Ordering::SeqCst) * 2)).step_by(2)
            {
                let lock = Arc::new(Mutex::new(0));
                let mut locks = Vec::new();
                locks.extend((0..nr_threads).into_iter().map(|_| lock.clone()));
                let handles: Vec<_> = locks
                    .into_iter()
                    .map(|lock| {
                        run_closure_in_new_thread(Priority::default_user(), move || {
                            for _ in 0..INNER_ITER {
                                let mut inner = lock.lock();
                                if quick_random() % 20 == 0 {
                                    let _ = sys_thread_sync(
                                        &mut [],
                                        Some(&mut Duration::from_millis(1)),
                                    );
                                }
                                *inner += 1;
                            }
                        })
                    })
                    .collect();

                for handle in handles {
                    handle.1.wait();
                }
                let inner = lock.lock();
                let val = *inner;
                drop(inner);
                assert_eq!(val, nr_threads * INNER_ITER);
            }
        }
    }
}
