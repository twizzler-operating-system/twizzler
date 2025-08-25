use intrusive_collections::{intrusive_adapter, LinkedList};

use crate::{
    spinlock::{LockGuard, SpinLoop},
    thread::{current_thread_ref, priority::Priority, Thread, ThreadRef},
};

pub const NR_QUEUES: usize = 32;
#[derive(Default)]
pub struct SchedulingQueues {
    pub queues: [LinkedList<SchedLinkAdapter>; NR_QUEUES],
    pub last_chosen_priority: Option<Priority>,
}

intrusive_adapter!(pub SchedLinkAdapter = ThreadRef: Thread { sched_link: intrusive_collections::linked_list::AtomicLink });

pub struct SchedLockGuard<'a> {
    pub(super) queues: LockGuard<'a, SchedulingQueues, SpinLoop>,
}

impl core::ops::Deref for SchedLockGuard<'_> {
    type Target = SchedulingQueues;
    fn deref(&self) -> &Self::Target {
        &*self.queues
    }
}

impl core::ops::DerefMut for SchedLockGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.queues
    }
}

impl Drop for SchedLockGuard<'_> {
    fn drop(&mut self) {
        current_thread_ref().map(|c| c.exit_critical());
    }
}

impl SchedulingQueues {
    pub fn reinsert_thread(&mut self, thread: ThreadRef) -> bool {
        let queue_number = thread.queue_number::<NR_QUEUES>();
        let needs_preempt = if let Some(ref last) = self.last_chosen_priority {
            last < &thread.effective_priority()
        } else {
            false
        };
        if thread.sched_link.is_linked() {
            panic!(
                "tried to reinsert thread that is already linked: {}",
                thread.id()
            );
        }
        self.queues[queue_number].push_back(thread);
        needs_preempt
    }

    pub fn check_priority_change(&mut self, thread: &Thread) -> bool {
        for i in 0..NR_QUEUES {
            let queue = &mut self.queues[i];

            let mut cursor = queue.front_mut();
            while let Some(item) = cursor.get() {
                if item.id() == thread.id() {
                    let item = cursor.remove().unwrap();
                    drop(cursor);
                    return self.reinsert_thread(item);
                }
                cursor.move_next();
            }
        }
        false
    }

    pub fn get_min_non_empty(&self) -> usize {
        for i in 0..NR_QUEUES {
            if !self.queues[i].is_empty() {
                return i;
            }
        }
        NR_QUEUES
    }

    pub fn has_work(&self) -> bool {
        self.get_min_non_empty() != NR_QUEUES || self.last_chosen_priority.is_some()
    }

    pub fn should_preempt(&self, pri: &Priority, eq: bool) -> bool {
        let q = pri.queue_number::<NR_QUEUES>();
        let m = self.get_min_non_empty();
        let c = self
            .last_chosen_priority
            .as_ref()
            .map_or(NR_QUEUES, |p| p.queue_number::<NR_QUEUES>());
        if eq {
            q <= m || q <= c
        } else {
            q < m || q < c
        }
    }

    pub fn has_higher_priority(&self, pri: Option<&Priority>) -> bool {
        let q = self.get_min_non_empty();
        if let Some(pri) = pri {
            let highest = todo!(); //Priority::from_queue_number::<NR_QUEUES>(q);
            &highest > pri
                || self
                    .last_chosen_priority
                    .as_ref()
                    .map_or(false, |last| last > pri)
        } else {
            q < NR_QUEUES || self.last_chosen_priority.is_some()
        }
    }

    pub fn choose_next(&mut self, for_self: bool) -> Option<ThreadRef> {
        for queue in &mut self.queues {
            if !queue.is_empty() {
                let choice = queue.pop_front();
                if for_self {
                    self.last_chosen_priority = choice.as_ref().map(|c| c.effective_priority());
                }
                return choice;
            }
        }
        if for_self {
            self.last_chosen_priority = None;
        }
        None
    }
}
