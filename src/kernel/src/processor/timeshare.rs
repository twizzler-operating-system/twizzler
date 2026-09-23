use core::fmt::Debug;

use intrusive_collections::LinkedList;

use super::rq::SchedLinkAdapter;
use crate::thread::{Thread, ThreadRef, priority::MAX_PRIORITY};

/// One slot per User priority value.
pub const SLOTS: usize = MAX_PRIORITY as usize;
/// Insert-offset advance per stattick: ULE moves 7/4 of a 109-slot ring per stathz tick, so a
/// batch thread waits at most about half a second; 2 of 128 keeps that.
const ADVANCE: usize = 2;

/// ULE's timeshare calendar. A thread is filed `MAX_PRIORITY - 1 - value` slots past the insert
/// offset, which advances with the statclock once the drain offset has caught up with it, so a
/// low value is a longer wait, not a permanent one. Own cache line; see `PriorityQueue` in `rq.rs`.
#[repr(align(64))]
pub(super) struct TimeshareQueue {
    count: usize,
    /// Queued threads per priority value, for `highest_priority`.
    priorities: [u16; SLOTS],
    ins_off: usize,
    deq_off: usize,
    queues: [LinkedList<SchedLinkAdapter>; SLOTS],
}

impl Debug for TimeshareQueue {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "ts {:5} ins {:3} deq {:3} [",
            self.count, self.ins_off, self.deq_off
        )?;
        for i in 0..SLOTS {
            let q = (self.deq_off + i) % SLOTS;
            let mut iter = self.queues[q].iter();
            if let Some(first) = iter.next() {
                let more = if iter.next().is_some() { "..." } else { "" };
                write!(f, " +{}:{}{}", i, first.id(), more)?;
            }
        }
        write!(f, " ]")
    }
}

fn norm(off: isize) -> usize {
    off.rem_euclid(SLOTS as isize) as usize
}

impl TimeshareQueue {
    pub const fn new() -> Self {
        const VAL: LinkedList<SchedLinkAdapter> = LinkedList::new(SchedLinkAdapter::NEW);
        Self {
            queues: [VAL; SLOTS],
            count: 0,
            ins_off: 0,
            deq_off: 0,
            priorities: [0; SLOTS],
        }
    }

    pub fn highest_priority(&self) -> Option<u16> {
        if self.is_empty() {
            return None;
        }
        (0..SLOTS)
            .rev()
            .find(|&v| self.priorities[v] > 0)
            .map(|v| v as u16)
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn tally(&mut self, th: &Thread, delta: i32) {
        let p = &mut self.priorities[th.get_stable_effective_priority().value as usize % SLOTS];
        *p = (*p as i32 + delta).max(0) as u16;
    }

    /// `preempted`: the thread lost its cpu before its slice ran out, so it goes into the slot
    /// being drained instead of paying the calendar again -- at its back, so the woken thread
    /// it made way for (`insert_front`) stays ahead of it.
    pub fn insert(&mut self, th: ThreadRef, preempted: bool) {
        let pri = th.stable_effective_priority();
        self.tally(&th, 1);
        self.count += 1;
        if preempted {
            self.queues[self.deq_off].push_back(th);
            return;
        }
        let mut idx = norm((self.ins_off + (MAX_PRIORITY - 1 - pri.value) as usize) as isize);
        // Never behind the drain offset: a low value that wraps into the span still to be
        // drained would run next, so it is clamped to the last slot before the drain instead.
        if self.deq_off != self.ins_off
            && norm(idx as isize - self.deq_off as isize)
                < norm(self.ins_off as isize - self.deq_off as isize)
        {
            idx = norm(self.deq_off as isize - 1);
        }
        self.queues[idx].push_back(th);
    }

    /// Ahead of everything queued: the next `take` returns `th` unless a later front insert
    /// lands first. For a woken thread, so a hand-off is not filed behind the preempted.
    pub fn insert_front(&mut self, th: ThreadRef) {
        self.tally(&th, 1);
        self.count += 1;
        self.queues[self.deq_off].push_front(th);
    }

    /// Move the drain offset past empty slots, up to the insert offset.
    fn advance_deq(&mut self) {
        while self.deq_off != self.ins_off && self.queues[self.deq_off].is_empty() {
            self.deq_off = (self.deq_off + 1) % SLOTS;
        }
    }

    pub fn take(&mut self) -> Option<ThreadRef> {
        for i in 0..SLOTS {
            let q = (self.deq_off + i) % SLOTS;
            if let Some(th) = self.queues[q].pop_front() {
                self.tally(&th, -1);
                self.count -= 1;
                self.advance_deq();
                return Some(th);
            }
        }
        None
    }

    /// Unlink `th` from whichever slot holds it, for a priority-driven re-file.
    pub fn remove_thread(&mut self, th: &Thread) -> Option<ThreadRef> {
        for q in 0..SLOTS {
            let mut cursor = self.queues[q].front_mut();
            while let Some(t) = cursor.get() {
                if core::ptr::eq(t as *const Thread, th as *const Thread) {
                    let t = cursor.remove()?;
                    self.tally(&t, -1);
                    self.count -= 1;
                    self.advance_deq();
                    return Some(t);
                }
                cursor.move_next();
            }
        }
        None
    }

    /// One statclock tick. The insert offset only moves once the drain has caught up with it,
    /// so a backlog freezes the calendar rather than letting new arrivals lap it.
    pub fn clock(&mut self) {
        if self.ins_off == self.deq_off {
            self.ins_off = (self.ins_off + ADVANCE) % SLOTS;
            self.advance_deq();
        }
    }
}

mod test {
    use alloc::sync::Arc;

    use twizzler_kernel_macros::kernel_test;

    use super::{SLOTS, TimeshareQueue};
    use crate::thread::{
        Thread, ThreadRef,
        priority::{MAX_PRIORITY, Priority, PriorityClass},
    };

    fn user(value: u16) -> ThreadRef {
        let thread = Arc::new(Thread::new(
            None,
            None,
            Priority {
                class: PriorityClass::User,
                value,
            },
        ));
        thread.set_name("idle");
        thread
    }

    #[kernel_test]
    fn test_higher_value_drains_first() {
        let mut ts = TimeshareQueue::new();
        let lo = user(10);
        let hi = user(100);
        ts.insert(lo.clone(), false);
        ts.insert(hi.clone(), false);
        assert_eq!(ts.highest_priority(), Some(100));
        assert_eq!(ts.take().unwrap().id(), hi.id());
        assert_eq!(ts.take().unwrap().id(), lo.id());
        assert!(ts.take().is_none());
        assert!(ts.is_empty());
    }

    #[kernel_test]
    fn test_preempted_goes_to_the_drain_slot() {
        let mut ts = TimeshareQueue::new();
        let a = user(64);
        let b = user(64);
        let p = user(1);
        ts.insert(a.clone(), false);
        ts.insert(b.clone(), false);
        ts.insert(p.clone(), true);
        let w = user(64);
        ts.insert_front(w.clone());
        assert_eq!(ts.take().unwrap().id(), w.id());
        assert_eq!(ts.take().unwrap().id(), p.id());
        assert_eq!(ts.take().unwrap().id(), a.id());
        assert_eq!(ts.take().unwrap().id(), b.id());
    }

    #[kernel_test]
    fn test_clock_rotates_and_low_value_never_wraps_ahead() {
        let mut ts = TimeshareQueue::new();
        let old = user(64);
        ts.insert(old.clone(), false);
        // Advance well past the old thread's slot, so it sits far behind the insert offset.
        for _ in 0..SLOTS {
            ts.clock();
        }
        let newer = user(0);
        ts.insert(newer.clone(), false);
        assert_eq!(ts.take().unwrap().id(), old.id());
        assert_eq!(ts.take().unwrap().id(), newer.id());
        let _ = MAX_PRIORITY;
    }
}
