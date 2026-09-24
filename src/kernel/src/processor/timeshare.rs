use core::fmt::Debug;

use intrusive_collections::LinkedList;

use super::{rq::SchedLinkAdapter, sched::wakestats};
use crate::thread::{Thread, ThreadRef, interact::BATCH_MAX, priority::MAX_PRIORITY};

/// One slot per User priority value.
pub const SLOTS: usize = MAX_PRIORITY as usize;
/// Insert-offset advance per stattick: ULE moves 7/4 of a 109-slot ring per stathz tick, so a
/// batch thread waits at most about half a second; 2 of 128 keeps that.
const ADVANCE: usize = 2;

/// ULE's timeshare calendar. A thread is filed `BATCH_MAX - value` slots past the insert offset,
/// which advances with the statclock once the drain offset has caught up with it, so a low value
/// is a longer wait, not a permanent one. Own cache line; see `PriorityQueue` in `rq.rs`.
#[repr(align(64))]
pub(super) struct TimeshareQueue {
    count: usize,
    /// Queued threads per priority value, for `highest_priority`; `pri_mask` has a bit per
    /// nonzero entry and `occupied` one per non-empty slot, so neither is a 128-way scan.
    priorities: [u16; SLOTS],
    pri_mask: u128,
    occupied: u128,
    ins_off: usize,
    deq_off: usize,
    queues: [LinkedList<SchedLinkAdapter>; SLOTS],
}

const _: () = assert!(SLOTS == 128);

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
            pri_mask: 0,
            occupied: 0,
        }
    }

    pub fn highest_priority(&self) -> Option<u16> {
        if self.pri_mask == 0 {
            return None;
        }
        Some((SLOTS as u32 - 1 - self.pri_mask.leading_zeros()) as u16)
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn tally(&mut self, th: &Thread, delta: i32) {
        let v = th.get_stable_effective_priority().value as usize % SLOTS;
        let p = &mut self.priorities[v];
        *p = (*p as i32 + delta).max(0) as u16;
        if *p > 0 {
            self.pri_mask |= 1 << v;
        } else {
            self.pri_mask &= !(1 << v);
        }
    }

    fn push(&mut self, idx: usize, th: ThreadRef, front: bool) {
        if front {
            self.queues[idx].push_front(th);
        } else {
            self.queues[idx].push_back(th);
        }
        self.occupied |= 1 << idx;
    }

    fn note_popped(&mut self, idx: usize) {
        if self.queues[idx].is_empty() {
            self.occupied &= !(1 << idx);
        }
    }

    /// Slots from `from` to the next occupied one, or to `to` if that is nearer.
    fn gap(&self, from: usize, to: usize) -> usize {
        let next = self.occupied.rotate_right(from as u32).trailing_zeros() as usize;
        next.min(norm(to as isize - from as isize))
    }

    /// `preempted`: the thread lost its cpu before its slice ran out, so it goes into the slot
    /// being drained instead of paying the calendar again -- at its back, so the woken thread
    /// it made way for (`insert_front`) stays ahead of it.
    pub fn insert(&mut self, th: ThreadRef, preempted: bool) {
        let pri = th.stable_effective_priority();
        self.tally(&th, 1);
        self.count += 1;
        if preempted {
            let deq = self.deq_off;
            self.note(&th, wakestats::NOTE_TS_DRAIN, deq);
            self.push(deq, th, false);
            return;
        }
        // From BATCH_MAX, not the top of the ring: only batch values reach the calendar, and
        // `MAX_PRIORITY - 1` left the nearest 44 slots unused and pushed every spinner far enough
        // out to be clamped behind the drain.
        let mut idx = norm((self.ins_off + BATCH_MAX.saturating_sub(pri.value) as usize) as isize);
        // Never behind the drain offset: a low value that wraps into the span still to be
        // drained would run next, so it is clamped to the last slot before the drain instead.
        if self.deq_off != self.ins_off
            && norm(idx as isize - self.deq_off as isize)
                < norm(self.ins_off as isize - self.deq_off as isize)
        {
            idx = norm(self.deq_off as isize - 1);
            self.note(&th, wakestats::NOTE_TS_CLAMPED, idx);
        } else {
            self.note(&th, wakestats::NOTE_TS_CALENDAR, idx);
        }
        self.push(idx, th, false);
    }

    /// Ahead of everything queued: the next `take` returns `th` unless a later front insert
    /// lands first. For a woken thread, so a hand-off is not filed behind the preempted.
    pub fn insert_front(&mut self, th: ThreadRef) {
        self.tally(&th, 1);
        self.count += 1;
        let deq = self.deq_off;
        self.note(&th, wakestats::NOTE_TS_FRONT, deq);
        self.push(deq, th, true);
    }

    fn note(&self, th: &Thread, kind: u8, idx: usize) {
        if crate::kdiag_wake() {
            th.sched.queue_note.store(
                wakestats::queue_note(kind, th, self.ins_off, self.deq_off, idx),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    /// Move the drain offset past empty slots, up to the insert offset.
    fn advance_deq(&mut self) {
        let step = self.gap(self.deq_off, self.ins_off);
        self.deq_off = (self.deq_off + step) % SLOTS;
    }

    pub fn take(&mut self) -> Option<ThreadRef> {
        if self.occupied == 0 {
            return None;
        }
        // The first occupied slot at or after the drain offset, wrapping.
        let next = self
            .occupied
            .rotate_right(self.deq_off as u32)
            .trailing_zeros() as usize;
        let q = (self.deq_off + next) % SLOTS;
        let th = self.queues[q].pop_front()?;
        self.note_popped(q);
        self.tally(&th, -1);
        self.count -= 1;
        self.advance_deq();
        Some(th)
    }

    /// Unlink `th` from whichever slot holds it, for a priority-driven re-file.
    pub fn remove_thread(&mut self, th: &Thread) -> Option<ThreadRef> {
        for q in 0..SLOTS {
            let mut cursor = self.queues[q].front_mut();
            while let Some(t) = cursor.get() {
                if core::ptr::eq(t as *const Thread, th as *const Thread) {
                    let t = cursor.remove()?;
                    self.note_popped(q);
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
        assert!(ts.take().is_none());
    }

    #[kernel_test]
    fn test_take_drains_every_slot() {
        let mut ts = TimeshareQueue::new();
        let values = [0u16, 1, 17, 64, 100, MAX_PRIORITY - 1, 64, 0];
        let mut n = 0;
        for (i, &v) in values.iter().enumerate() {
            ts.insert(user(v), i % 3 == 1);
            n += 1;
            for _ in 0..(i * 7) {
                ts.clock();
            }
            if i == 4 {
                ts.insert_front(user(50));
                n += 1;
            }
        }
        for _ in 0..n {
            assert!(
                ts.take().is_some(),
                "take returned None with {} queued",
                ts.count
            );
        }
        assert!(ts.take().is_none());
        assert!(ts.is_empty());
        assert_eq!(ts.highest_priority(), None);
    }
}
