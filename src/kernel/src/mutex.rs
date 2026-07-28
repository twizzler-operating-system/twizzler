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

// TODO: reenable priority donation, and make it cheaper.

use core::{cell::UnsafeCell, panic::Location, sync::atomic::AtomicU64, time::Duration};

use intrusive_collections::{LinkedList, intrusive_adapter};
use twizzler_abi::{syscall::LockStats, thread::ExecutionState};

use crate::{
    idcounter::StableId,
    instant::Instant,
    once::Once,
    pager::check_timed_out_requests,
    processor::sched::schedule_thread,
    spinlock::Spinlock,
    syscall::sync::finish_blocking,
    thread::{
        Thread, ThreadRef, current_thread_ref,
        locktrack::{check_timed_out_mutexes, with_lock_tracker},
        priority::Priority,
    },
    time::TimeStatCollector,
};

#[repr(align(64))]
struct AlignedAtomicU64(AtomicU64);
struct SleepQueue {
    queue: LinkedList<MutexLinkAdapter>,
    pri: Option<Priority>,
    owner: Option<ThreadRef>,
    owned: bool,
    /// When true, release() has handed ownership to a waiter but that waiter
    /// hasn't called lock() yet. Prevents the releasing thread from re-acquiring
    /// and starving waiters (Bug #9 fairness fix).
    handoff: bool,
}

struct MutexStatCollector {
    nr_locks: usize,
    lock_time: TimeStatCollector,
    hold_time: TimeStatCollector,
}

static MUTEX_STATS: Once<Spinlock<MutexStatCollector>> = Once::new();

fn get_mutex_stats() -> &'static Spinlock<MutexStatCollector> {
    MUTEX_STATS.call_once(|| {
        Spinlock::new(MutexStatCollector {
            nr_locks: 0,
            lock_time: TimeStatCollector::new(),
            hold_time: TimeStatCollector::new(),
        })
    })
}

fn add_hold_time_sample(sample: Duration) {
    get_mutex_stats().lock().hold_time.add_sample(sample.into());
}

fn add_lock_time_sample(sample: Duration) {
    let mut stats = get_mutex_stats().lock();
    stats.nr_locks += 1;
    stats.lock_time.add_sample(sample.into());
}

pub fn get_lock_stats() -> LockStats {
    let stats = get_mutex_stats().lock();
    LockStats {
        mutex_lock_count: stats.nr_locks,
        mutex_waiting_count: 0,
        mutex_avg_waiting_time: stats.lock_time.get_stats(),
        mutex_hold_time: stats.hold_time.get_stats(),
    }
}

intrusive_adapter!(pub MutexLinkAdapter = ThreadRef: Thread { mutex_link: intrusive_collections::linked_list::AtomicLink });

/// A container data structure to manage mutual exclusion.
pub struct Mutex<T> {
    queue: Spinlock<SleepQueue>,
    cell: UnsafeCell<T>,
    locked_at: UnsafeCell<&'static Location<'static>>,
    safe_with_spinlocks: bool,
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
                handoff: false,
            }),
            cell: UnsafeCell::new(data),
            locked_at: UnsafeCell::new(Location::caller()),
            safe_with_spinlocks: false,
        }
    }

    pub fn set_safe_with_spinlocks(&mut self, safe: bool) {
        self.safe_with_spinlocks = safe;
    }

    /// Get a mut reference to the contained data. Does not perform locking, but is safe because we
    /// have a mut reference to the mutex itself.
    pub fn get_mut(&mut self) -> &mut T {
        self.cell.get_mut()
    }

    /// Lock the mutex and return a lock guard to manage a reference to the managed data. When the
    /// lock guard goes out of scope, the lock will be released.
    #[track_caller]
    pub fn lock(&self) -> LockGuard<'_, T> {
        let start_time = Instant::now();
        let caller = core::panic::Location::caller();
        let current_thread = current_thread_ref();
        let current_donated_priority = current_thread
            .as_ref()
            .and_then(|t| t.get_donated_priority());

        with_lock_tracker(|lt| {
            if !self.safe_with_spinlocks {
                assert!(
                    lt.spinlock_count() == 0,
                    "cannot lock mutex while holding spinlock (called from {})",
                    caller
                );
            }
            lt.intend_to_lock_mutex(caller, start_time)
        });

        if let Some(ref current_thread) = current_thread {
            if current_thread.is_critical() {
                panic!(
                    "cannot lock mutex in critical context (called from {})",
                    caller
                );
            }
            assert!(!current_thread.is_critical());
            assert!(
                !current_thread.mutex_link.is_linked(),
                "cannot lock mutex while waiting for another mutex (called from {}), ct = {}, owner = {}",
                caller,
                current_thread.id(),
                self.queue.lock().locker
            );
        }

        let int_state = crate::interrupt::disable();
        let mut i = 0;
        loop {
            i += 1;
            if i == 1000 {
                emerglogln!(
                    "mutex pause: {:?}: {} ({:?})",
                    caller,
                    i,
                    current_thread.as_ref().map(|t| t.is_idle_thread())
                );
                current_thread_ref().map(|ct| ct.print_locks());
            }
            if i % 10000 == 0 {
                //check_timed_out_mutexes();
                with_lock_tracker(|lt| lt.clear_intended_mutex());
                check_timed_out_requests();
                with_lock_tracker(|lt| lt.intend_to_lock_mutex(caller, start_time));
            }
            let guard = current_thread.as_ref().map(|ct| ct.enter_critical());
            let _reinsert = {
                let mut queue = self.queue.lock();
                if !queue.owned {
                    queue.owned = true;
                    if let Some(ref thread) = current_thread {
                        if let Some(ref pri) = queue.pri {
                            thread.donate_priority(pri.clone());
                        }
                    }

                    queue.owner = current_thread.cloned();
                    unsafe { self.locked_at.get().write(caller) };
                    break;
                } else if let Some(ref cur_owner) = queue.owner {
                    with_lock_tracker(|lt| {
                        lt.intended_mutex_owned_by(cur_owner.id(), unsafe {
                            self.locked_at.get().read()
                        });
                    });
                    if let Some(ref cur_thread) = current_thread {
                        if cur_thread.id() == cur_owner.id() {
                            if queue.handoff {
                                // This thread was handed ownership by release(). Clear
                                // the handoff flag and proceed as the new owner.
                                queue.handoff = false;
                                unsafe { self.locked_at.get().write(caller) };
                                break;
                            }
                            crate::panic::backtrace(false, None);
                            panic!("this mutex is not re-entrant");
                        }
                    }
                }

                let mut reinsert = true;
                if let Some(thread) = current_thread {
                    thread.set_mutex_wait(true);
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
            crate::arch::processor::spin_wait_iteration();
            if let Some(guard) = guard {
                finish_blocking(guard);
                if let Some(ct) = current_thread_ref() {
                    assert!(ct.get_mutex_wait());
                    assert!(!ct.mutex_link.is_linked());
                }
            }
        }

        if let Some(ct) = current_thread_ref() {
            assert!(!ct.mutex_link.is_linked());
            ct.set_mutex_wait(false);
        }

        let end_time = Instant::now();
        add_lock_time_sample(end_time - start_time);
        let tracker_index = with_lock_tracker(|lt| lt.record_mutex_lock());
        crate::interrupt::set(int_state);
        LockGuard {
            lock: self,
            prev_donated_priority: current_donated_priority,
            start_time,
            tracker_index,
        }
    }

    fn release(&self) {
        let mut queue = self.queue.lock();
        let g = current_thread_ref().map(|ct| ct.enter_critical());
        if let Some(ct) = current_thread_ref() {
            assert!(!ct.mutex_link.is_linked());
        }
        if let Some(thread) = queue.queue.pop_front() {
            // Hand off ownership directly to the next waiter instead of releasing.
            // This prevents the current thread from immediately re-acquiring and
            // starving waiters (Bug #9 fairness fix).
            queue.owner = Some(thread.clone());
            queue.handoff = true;
            // queue.owned stays true
            // Transfer the pending priority donation to the new owner, so it starts
            // running at the correct priority immediately. This prevents priority
            // inversion between schedule_thread() and the new owner's lock() call.
            if let Some(ref pri) = queue.pri {
                thread.donate_priority(pri.clone());
            }
            schedule_thread(thread);
            // Recalculate queue.pri for remaining waiters after popping.
            queue.pri = if queue.queue.is_empty() {
                None
            } else {
                Some(
                    queue
                        .queue
                        .iter()
                        .map(|t| t.effective_priority())
                        .max()
                        .unwrap(),
                )
            };
            drop(queue);
        } else {
            queue.owner = None;
            queue.owned = false;
            queue.handoff = false;
            queue.pri = None;
        }
        drop(g);
    }
}

/// Manages a reference to the data controlled by a mutex.
pub struct LockGuard<'a, T> {
    lock: &'a Mutex<T>,
    prev_donated_priority: Option<Priority>,
    start_time: Instant,
    tracker_index: Option<usize>,
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
                thread.remove_donated_priority();
                thread.donate_priority(prev.clone());
            }
        } else if let Some(thread) = current_thread_ref() {
            thread.remove_donated_priority();
        }
        if let Some(index) = self.tracker_index {
            with_lock_tracker(|lt| lt.record_mutex_unlock(index));
        }
        self.lock.release();
        let end_time = Instant::now();
        add_hold_time_sample(end_time - self.start_time);
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
        processor::mp::NR_CPUS,
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
                        run_closure_in_new_thread(Priority::USER, move || {
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
