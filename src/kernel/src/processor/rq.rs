use intrusive_collections::{intrusive_adapter, LinkedList};

use super::timeshare::TimeshareQueue;
use crate::{
    spinlock::{GenericSpinlock, LockGuard, SpinLoop},
    thread::{current_thread_ref, priority::PriorityClass, Thread, ThreadRef},
};

#[repr(transparent)]
struct SchedSpinlock<T>(GenericSpinlock<T, SpinLoop>);

impl<T> SchedSpinlock<T> {
    fn lock(&self) -> SchedLockGuard<'_, T> {
        current_thread_ref().map(|c| c.enter_critical_unguarded());
        let queue = self.0.lock();
        SchedLockGuard { queue }
    }
}

pub struct RunQueue<const N: usize> {
    realtime: SchedSpinlock<PriorityQueue<N>>,
    timeshare: SchedSpinlock<TimeshareQueue<N>>,
    idle: SchedSpinlock<PriorityQueue<N>>,
}

pub struct SchedLockGuard<'a, T> {
    pub(super) queue: LockGuard<'a, T, SpinLoop>,
}

impl<T> core::ops::Deref for SchedLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &*self.queue
    }
}

impl<T> core::ops::DerefMut for SchedLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.queue
    }
}

impl<T> Drop for SchedLockGuard<'_, T> {
    fn drop(&mut self) {
        current_thread_ref().map(|c| c.exit_critical());
    }
}

struct PriorityQueue<const N: usize> {
    count: usize,
    queues: [LinkedList<SchedLinkAdapter>; N],
}

impl<const N: usize> PriorityQueue<N> {
    const fn new() -> Self {
        const VAL: LinkedList<SchedLinkAdapter> = LinkedList::new(SchedLinkAdapter::NEW);
        Self {
            queues: [VAL; N],
            count: 0,
        }
    }

    fn insert(&mut self, th: ThreadRef) {
        let priority = th.effective_priority();
        let q = if priority.class == PriorityClass::User {
            // This must be a user thread getting a deadline boost.
            N - 1
        } else {
            priority.value as usize / N
        };
        self.queues[q].push_back(th);
        self.count += 1;
    }

    fn take(&mut self) -> Option<ThreadRef> {
        if self.count == 0 {
            return None;
        }
        for q in 0..N {
            if let Some(th) = self.queues[q].pop_front() {
                self.count -= 1;
                return Some(th);
            }
        }

        None
    }
}

intrusive_adapter!(pub SchedLinkAdapter = ThreadRef: Thread { sched_link: intrusive_collections::linked_list::AtomicLink });

impl<const N: usize> RunQueue<N> {
    pub fn new() -> Self {
        Self {
            realtime: SchedSpinlock(GenericSpinlock::new(PriorityQueue::new())),
            timeshare: SchedSpinlock(GenericSpinlock::new(TimeshareQueue::new())),
            idle: SchedSpinlock(GenericSpinlock::new(PriorityQueue::new())),
        }
    }

    pub fn insert(&self, th: ThreadRef) -> bool {
        match th.effective_priority().class {
            PriorityClass::Realtime => {
                self.realtime.lock().insert(th);
                true
            }
            PriorityClass::User => {
                let is_thread_deadline = todo!();
                if is_thread_deadline {
                    self.realtime.lock().insert(th);
                } else {
                    self.timeshare.lock().insert(th);
                }
                true
            }
            _ => {
                self.idle.lock().insert(th);
                false
            }
        }
    }

    pub fn take(&self) -> Option<ThreadRef> {
        if let Some(th) = self.realtime.lock().take() {
            return Some(th);
        }
        if let Some(th) = self.timeshare.lock().take() {
            return Some(th);
        }
        if let Some(th) = self.idle.lock().take() {
            return Some(th);
        }
        None
    }
}
