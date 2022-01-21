use alloc::{sync::Arc, vec::Vec};
use nonoverlapping_interval_tree::{IntervalValue, NonOverlappingIntervalTree};

use crate::mutex::Mutex;

use super::{
    pages::{Page, PageRef},
    pagevec::{PageVec, PageVecRef},
    PageNumber,
};

pub struct PageRange {
    pub start: PageNumber,
    pub length: usize,
    pub offset: usize,
    pv: PageVecRef,
}

impl PageRange {
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

    pub fn pv_ref_count(&self) -> usize {
        Arc::strong_count(&self.pv)
    }

    pub fn is_shared(&self) -> bool {
        self.pv_ref_count() > 1
    }

    pub fn gc_pagevec(&self) {
        todo!()
    }

    pub fn split_at(&self, pn: PageNumber) -> (Option<PageRange>, PageRange, Option<PageRange>) {
        assert!(pn >= self.start);
        assert!(pn < self.start.offset(self.length));

        let r1 = if pn > self.start {
            let diff = pn - self.start;
            Some(self.new_from(self.start, self.offset, diff))
        } else {
            None
        };

        let r3 = if pn < self.start.offset(self.length - 1) {
            let off = pn.offset(1) - self.start;
            Some(self.new_from(
                pn.offset(1),
                self.offset + off,
                self.start.offset(self.length - 1) - pn,
            ))
        } else {
            None
        };

        let r2 = self.new_from(pn, self.offset + (pn - self.start), 1);

        (r1, r2, r3)
    }

    pub fn copy_pv(&mut self) {
        let new_pv = PageVec::new();
        self.pv = Arc::new(Mutex::new(new_pv));
    }

    pub fn range(&self) -> core::ops::Range<PageNumber> {
        self.start..self.start.offset(self.length)
    }
}

pub struct PageRangeTree {
    tree: NonOverlappingIntervalTree<PageNumber, PageRange>,
}

impl PageRangeTree {
    pub fn new() -> Self {
        Self {
            tree: NonOverlappingIntervalTree::new(),
        }
    }

    pub fn get(&self, pn: PageNumber) -> Option<&PageRange> {
        self.tree.get(&pn)
    }

    pub fn get_mut(&mut self, pn: PageNumber) -> Option<&mut PageRange> {
        self.tree.get_mut(&pn)
    }

    pub fn get_page(&mut self, pn: PageNumber, is_write: bool) -> Option<(PageRef, bool)> {
        let range = self.get(pn)?;
        let page = range.get_page(pn);
        let shared = range.is_shared();
        if !shared || !is_write {
            return Some((page, shared));
        }
        let range = self.tree.remove(&pn).unwrap();
        /* need to copy */
        let (r1, mut r2, r3) = range.split_at(pn);
        /* r2 is always the one we want */
        r2.copy_pv();

        if let Some(r1) = r1 {
            let res = self.insert_replace(r1.range(), r1);
            assert_eq!(res.len(), 0);
        }

        let res = self.insert_replace(r2.range(), r2);
        assert_eq!(res.len(), 0);

        if let Some(r3) = r3 {
            let res = self.insert_replace(r3.range(), r3);
            assert_eq!(res.len(), 0);
        }

        let range = self.get(pn)?;
        let page = range.get_page(pn);
        let shared = range.is_shared();
        assert_eq!(shared, false);
        Some((page, false))
    }

    pub fn insert_replace(
        &mut self,
        k: core::ops::Range<PageNumber>,
        r: PageRange,
    ) -> Vec<(core::ops::Range<PageNumber>, PageRange)> {
        self.tree.insert_replace(k, r)
    }

    pub fn range(
        &self,
        r: core::ops::Range<PageNumber>,
    ) -> nonoverlapping_interval_tree::ValueRange<
        '_,
        PageNumber,
        IntervalValue<PageNumber, PageRange>,
    > {
        self.tree.range(r)
    }

    pub fn range_mut(
        &mut self,
        r: core::ops::Range<PageNumber>,
    ) -> nonoverlapping_interval_tree::ValueRangeMut<
        '_,
        PageNumber,
        IntervalValue<PageNumber, PageRange>,
    > {
        self.tree.range_mut(r)
    }

    pub fn add_page(&mut self, pn: PageNumber, page: Page) {
        let range = self.tree.get(&pn);
        if let Some(range) = range {
            range.add_page(pn, page);
        } else {
            let range = PageRange::new(pn);
            range.add_page(pn, page);
            self.tree.insert_replace(pn..pn.next(), range);
        }
    }
}
