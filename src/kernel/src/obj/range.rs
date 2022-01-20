use alloc::{sync::Arc, vec::Vec};
use nonoverlapping_interval_tree::{IntervalValue, NonOverlappingIntervalTree};

use crate::mutex::Mutex;

use super::{
    pages::{Page, PageRef},
    pagevec::{PageVec, PageVecRef},
    PageNumber,
};

pub struct Range {
    pub start: PageNumber,
    pub length: usize,
    pub offset: usize,
    pv: PageVecRef,
}

impl Range {
    fn new(start: PageNumber) -> Self {
        Self {
            start,
            length: 0,
            offset: 0,
            pv: Arc::new(Mutex::new(PageVec::new())),
        }
    }

    pub fn new_from(&self, new_start: PageNumber, new_offset: usize, new_length: usize) -> Self {
        Self {
            start: new_start,
            length: new_length,
            offset: new_offset,
            pv: self.pv.clone(),
        }
    }

    fn get_page(&self, pn: PageNumber) -> PageRef {
        assert!(pn >= self.start);
        let off = pn - self.start;
        self.pv.lock().get_page(self.offset + off)
    }

    fn add_page(&self, pn: PageNumber, page: Page) {
        assert!(pn >= self.start);
        let off = pn - self.start;
        self.pv.lock().add_page(self.offset + off, page);
    }
}

pub struct RangeTree {
    tree: NonOverlappingIntervalTree<PageNumber, Range>,
}

impl RangeTree {
    pub fn new() -> Self {
        Self {
            tree: NonOverlappingIntervalTree::new(),
        }
    }

    pub fn get(&self, pn: PageNumber) -> Option<&Range> {
        self.tree.get(&pn)
    }

    pub fn get_mut(&mut self, pn: PageNumber) -> Option<&mut Range> {
        self.tree.get_mut(&pn)
    }

    pub fn get_page(&self, pn: PageNumber) -> Option<PageRef> {
        let range = self.get(pn)?;
        Some(range.get_page(pn))
    }

    pub fn insert_replace(
        &mut self,
        k: core::ops::Range<PageNumber>,
        r: Range,
    ) -> Vec<(core::ops::Range<PageNumber>, Range)> {
        self.tree.insert_replace(k, r)
    }

    pub fn range(
        &self,
        r: core::ops::Range<PageNumber>,
    ) -> nonoverlapping_interval_tree::ValueRange<'_, PageNumber, IntervalValue<PageNumber, Range>>
    {
        self.tree.range(r)
    }

    pub fn range_mut(
        &mut self,
        r: core::ops::Range<PageNumber>,
    ) -> nonoverlapping_interval_tree::ValueRangeMut<'_, PageNumber, IntervalValue<PageNumber, Range>>
    {
        self.tree.range_mut(r)
    }

    pub fn add_page(&mut self, pn: PageNumber, page: Page) {
        let range = self.tree.get(&pn);
        if let Some(range) = range {
            range.add_page(pn, page);
        } else {
            let range = Range::new(pn);
            range.add_page(pn, page);
            self.tree.insert_replace(pn..pn.next(), range);
        }
    }
}
