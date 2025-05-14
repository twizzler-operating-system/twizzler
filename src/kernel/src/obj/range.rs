use alloc::{sync::Arc, vec::Vec};
use core::fmt::Display;

use nonoverlapping_interval_tree::{IntervalValue, NonOverlappingIntervalTree};

use super::{
    pages::{Page, PageRef},
    pagevec::{PageVec, PageVecRef},
    PageNumber,
};
use crate::{memory::tracker::FrameAllocator, mutex::Mutex};

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

    fn try_get_page(&self, pn: PageNumber) -> Option<PageRef> {
        assert!(pn >= self.start);
        let off = pn - self.start;
        self.pv.lock().try_get_page(self.offset + off)
    }

    fn add_page(&self, pn: PageNumber, page: Page) -> Arc<Page> {
        assert!(pn >= self.start);
        assert!(pn < self.start.offset(self.length));
        let off = pn - self.start;
        self.pv.lock().add_page(self.offset + off, page)
    }

    pub fn pv_ref_count(&self) -> usize {
        Arc::strong_count(&self.pv)
    }

    pub fn is_shared(&self) -> bool {
        self.pv_ref_count() > 1
    }

    pub fn gc_pagevec(&mut self) {
        if self.is_shared() {
            // TODO: maybe we can do something smarter here, but it may be dangerous. In particular,
            // we should study what pagevecs actually look like in a long-running system
            // and decide what to do based on that. Of course, if we want to be able to
            // do anything here, we'll either need to promote pagevecs to non-shared
            // or we will need to track more page info.
            return;
        }

        let mut pv = self.pv.lock();
        pv.truncate_and_drain(self.offset, self.length);
        self.offset = 0;
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

impl Display for PageRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{{{}, {}, {}}} -> {} {}",
            self.start,
            self.length,
            self.offset,
            if self.is_shared() { "s" } else { "p" },
            self.pv.lock().show_part(self)
        )
    }
}

#[derive(Default)]
pub struct PageRangeTree {
    tree: NonOverlappingIntervalTree<PageNumber, PageRange>,
}

#[derive(Debug)]
pub enum PageStatus {
    Ready(PageRef, bool),
    NoPage,
    AllocFail,
    DataFail,
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

    pub fn remove(&mut self, pn: &PageNumber) -> Option<PageRange> {
        self.tree.remove(pn)
    }

    fn split_into_three(
        &mut self,
        pn: PageNumber,
        discard: bool,
        allocator: &mut FrameAllocator,
    ) -> bool {
        let Some(range) = self.tree.remove(&pn) else {
            // No work to do
            return true;
        };
        let (r1, mut r2, r3) = range.split_at(pn);
        /* r2 is always the one we want */
        let pv = if discard {
            PageVec::new()
        } else {
            let Some(pv) = r2
                .pv
                .lock()
                .clone_pages_limited(r2.offset, r2.length, allocator)
            else {
                // Failed to allocate pages, restore the tree.
                self.tree.insert(range.range(), range);
                return false;
            };
            pv
        };

        r2.pv = Arc::new(Mutex::new(pv));
        r2.offset = 0;

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
        true
    }

    fn try_do_get_page(&self, pn: PageNumber) -> Option<(PageRef, bool)> {
        let range = self.get(pn)?;
        let page = range.try_get_page(pn)?;
        let shared = range.is_shared();
        Some((page, shared))
    }

    pub fn get_page(
        &mut self,
        pn: PageNumber,
        is_write: bool,
        allocator: Option<&mut FrameAllocator>,
    ) -> PageStatus {
        let Some((page, shared)) = self.try_do_get_page(pn) else {
            return PageStatus::NoPage;
        };
        if !shared || !is_write {
            return PageStatus::Ready(page, shared);
        }
        if let Some(allocator) = allocator {
            if !self.split_into_three(pn, false, allocator) {
                return PageStatus::AllocFail;
            }
        }
        let Some((page, shared)) = self.try_do_get_page(pn) else {
            return PageStatus::NoPage;
        };
        assert!(!shared);
        PageStatus::Ready(page, false)
    }

    pub fn try_get_page(&mut self, pn: PageNumber) -> PageStatus {
        let Some((page, shared)) = self.try_do_get_page(pn) else {
            return PageStatus::NoPage;
        };
        PageStatus::Ready(page, shared)
    }

    pub fn insert_replace(
        &mut self,
        k: core::ops::Range<PageNumber>,
        r: PageRange,
    ) -> Vec<(core::ops::Range<PageNumber>, PageRange)> {
        assert_ne!(r.length, 0);
        assert_ne!(k.start, k.end);
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

    pub fn gc_tree(&mut self) {
        todo!()
    }

    pub fn add_page(
        &mut self,
        pn: PageNumber,
        page: Page,
        allocator: Option<&mut FrameAllocator>,
    ) -> Option<Arc<Page>> {
        const MAX_EXTENSION_ALLOWED: usize = 16;
        let range = self.tree.get(&pn);
        if let Some(mut range) = range {
            if range.is_shared() {
                if let Some(allocator) = allocator {
                    if !self.split_into_three(pn, true, allocator) {
                        return None;
                    }
                }
                range = self.tree.get(&pn).unwrap();
            }
            Some(range.add_page(pn, page))
        } else {
            // Try to extend a previous range.
            if let Some((_, prev_range)) =
                self.tree.range_mut(PageNumber::from_offset(0)..pn).last()
            {
                let end = prev_range.start.offset(prev_range.length - 1);
                let diff = pn - end;
                if !prev_range.is_shared() && diff <= MAX_EXTENSION_ALLOWED {
                    let mut prev_range = self.tree.remove(&end).unwrap();
                    prev_range.length += diff;
                    let p = prev_range.add_page(pn, page);
                    let kicked = self.tree.insert_replace(prev_range.range(), prev_range);
                    assert_eq!(kicked.len(), 0);
                    return Some(p);
                }
            }
            let mut range = PageRange::new(pn);
            range.length = 1;
            let p = range.add_page(pn, page);
            let kicked = self.tree.insert_replace(pn..pn.next(), range);
            assert_eq!(kicked.len(), 0);
            Some(p)
        }
    }

    pub fn print_tree(&self) {
        let r = self.range(0.into()..usize::MAX.into());
        for range in r {
            let val = range.1.value();
            logln!(
                "  range [{}, {}) => {}",
                range.0.num(),
                range.0.offset(range.1.length).num(),
                val
            );
        }
    }
}
