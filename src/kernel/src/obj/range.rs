use nonoverlapping_interval_tree::NonOverlappingIntervalTree;

use super::{pages::PageRef, pagevec::PageVecRef, PageNumber};

pub struct Range {
    start: PageNumber,
    length: usize,
    offset: usize,
    pv: PageVecRef,
}

impl Range {
    pub fn get_page(&self, pn: PageNumber) -> PageRef {
        assert!(pn >= self.start);
        let off = pn - self.start;
        self.pv.lock().get_page(self.offset + off)
    }
}

pub struct RangeTree {
    tree: NonOverlappingIntervalTree<PageNumber, Range>,
}

impl RangeTree {
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
}
