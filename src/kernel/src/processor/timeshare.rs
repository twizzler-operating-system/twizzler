use intrusive_collections::LinkedList;

use super::rq::SchedLinkAdapter;
use crate::thread::ThreadRef;

pub(super) struct TimeshareQueue<const N: usize> {
    count: usize,
    insert_idx: usize,
    take_idx: usize,
    queues: [LinkedList<SchedLinkAdapter>; N],
}

impl<const N: usize> TimeshareQueue<N> {
    pub const fn new() -> Self {
        const VAL: LinkedList<SchedLinkAdapter> = LinkedList::new(SchedLinkAdapter::NEW);
        Self {
            queues: [VAL; N],
            count: 0,
            insert_idx: 0,
            take_idx: 0,
        }
    }

    pub fn insert(&mut self, th: ThreadRef) {
        let prio_idx_offset: usize = todo!();
        let q = (self.insert_idx + prio_idx_offset) % N;
        self.queues[q].push_back(th);
        self.count += 1;
    }

    pub fn take(&mut self) -> Option<ThreadRef> {
        for i in 0..N {
            let q = (self.take_idx + i) % N;
            if let Some(th) = self.queues[q].pop_front() {
                self.take_idx = q;
                self.count -= 1;
                return Some(th);
            }
            if q == self.insert_idx {
                self.take_idx = q;
                break;
            }
        }
        None
    }

    pub fn advance_insert_index(&mut self, steps: usize, force: bool) {
        if self.insert_idx != self.take_idx && !force {
            return;
        }
        self.insert_idx = (self.insert_idx + steps) % N;
    }
}
