use core::fmt::Debug;

use intrusive_collections::LinkedList;

use super::rq::SchedLinkAdapter;
use crate::thread::{priority::MAX_PRIORITY, ThreadRef};

pub(super) struct TimeshareQueue<const N: usize> {
    count: usize,
    priorities: [u32; N],
    insert_idx: usize,
    take_idx: usize,
    queues: [LinkedList<SchedLinkAdapter>; N],
}

impl<const N: usize> Debug for TimeshareQueue<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ts {:5} [", self.count)?;
        for i in 0..N {
            if i != 0 {
                write!(f, " |")?;
            }
            if self.take_idx == self.insert_idx && self.take_idx == i {
                write!(f, "#")?;
            } else if self.take_idx == i {
                write!(f, "~")?;
            } else if self.insert_idx == i {
                write!(f, ">")?;
            } else {
                write!(f, " ")?;
            }
            let mut iter = self.queues[i].iter();
            if let Some(first) = iter.next() {
                if iter.next().is_some() {
                    write!(f, "{:5}...", first.id())?;
                } else {
                    write!(f, "{:5}   ", first.id())?;
                }
            } else {
                write!(f, "        ",)?;
            }
        }
        write!(f, "]")?;

        Ok(())
    }
}

impl<const N: usize> TimeshareQueue<N> {
    pub const fn new() -> Self {
        const VAL: LinkedList<SchedLinkAdapter> = LinkedList::new(SchedLinkAdapter::NEW);
        Self {
            queues: [VAL; N],
            count: 0,
            insert_idx: 0,
            take_idx: 0,
            priorities: [0; N],
        }
    }

    pub fn highest_priority(&self) -> Option<u16> {
        if self.is_empty() {
            return None;
        }
        for i in (0..N).rev() {
            if self.priorities[i] > 0 {
                return Some((i * (MAX_PRIORITY as usize / N)) as u16);
            }
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn insert(&mut self, th: ThreadRef, current: bool) {
        let pri = th.stable_effective_priority();
        let q = if current {
            self.take_idx
        } else {
            let prio_idx_offset: usize =
                (MAX_PRIORITY - pri.value) as usize / (MAX_PRIORITY as usize / N);
            let q = (self.insert_idx + prio_idx_offset) % N;
            if q == self.take_idx && self.take_idx != self.insert_idx {
                q.checked_sub(1).unwrap_or(N - 1)
            } else {
                q
            }
        };
        log::trace!(
            "insert thread {},{}: {} {} {}",
            th.id(),
            current,
            q,
            self.take_idx,
            self.insert_idx
        );
        self.queues[q].push_back(th);
        self.priorities[pri.value as usize / (MAX_PRIORITY as usize / N)] += 1;
        self.count += 1;
    }

    pub fn take(&mut self) -> Option<ThreadRef> {
        for i in 0..N {
            let q = (self.take_idx + i) % N;
            if let Some(th) = self.queues[q].pop_front() {
                self.take_idx = q;
                self.priorities[th.get_stable_effective_priority().value as usize
                    / (MAX_PRIORITY as usize / N)] -= 1;
                self.count -= 1;
                if self.take_idx != self.insert_idx && self.queues[q].is_empty() {
                    self.take_idx = (self.take_idx + 1) % N;
                }
                log::trace!(
                    "take thread {}: {} {} {}",
                    th.id(),
                    q,
                    self.take_idx,
                    self.insert_idx
                );
                return Some(th);
            }
        }
        // We found nothing. Reset the take pointer.
        self.take_idx = self.insert_idx;
        None
    }

    pub fn advance_insert_index(&mut self, steps: u64, force: bool) {
        if self.is_empty() {
            return;
        }
        log::trace!(
            "adv_insert {},{}: {} {}",
            steps,
            force,
            self.take_idx,
            self.insert_idx,
        );
        if self.insert_idx != self.take_idx && !force {
            return;
        }
        self.insert_idx = (self.insert_idx + steps as usize) % N;
    }
}
