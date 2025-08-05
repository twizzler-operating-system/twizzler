use alloc::{borrow::ToOwned, sync::Arc, vec::Vec};
use core::fmt::Display;

use nonoverlapping_interval_tree::{IntervalValue, NonOverlappingIntervalTree};
use twizzler_abi::object::ObjID;

use super::{pages::PageRef, pagevec::PageVecRef, PageNumber};
use crate::{
    condvar::CondVar,
    memory::tracker::FrameAllocator,
    mutex::Mutex,
    obj::{pages::Page, pagevec::PageVec},
    spinlock::Spinlock,
};

pub struct RangeSleep {
    wait: CondVar,
    locked: Spinlock<bool>,
}

impl RangeSleep {
    fn new() -> Self {
        Self {
            wait: CondVar::new(),
            locked: Spinlock::new(false),
        }
    }

    pub fn wait(&self) {
        loop {
            let guard = self.locked.lock();
            if !*guard {
                break;
            }
            self.wait.wait(guard);
        }
    }

    pub fn set_lock(&self) {
        *self.locked.lock() = true;
    }

    pub fn reset_lock(&self) {
        *self.locked.lock() = false;
        self.wait.signal();
    }
}

#[derive(Clone)]
enum BackingPages {
    Nothing,
    Single(PageRef),
    Many(PageVecRef),
}

pub struct PageRange {
    pub start: PageNumber,
    pub length: usize,
    pub offset: usize,
    backing: BackingPages,
    sleep: Option<Arc<RangeSleep>>,
}

impl PageRange {
    fn new(start: PageNumber) -> Self {
        Self {
            start,
            length: 0,
            offset: 0,
            backing: BackingPages::Nothing,
            sleep: None,
        }
    }

    pub fn new_from(&self, new_start: PageNumber, new_offset: usize, new_length: usize) -> Self {
        Self {
            start: new_start,
            length: new_length,
            offset: new_offset,
            backing: self.backing.clone(),
            sleep: None,
        }
    }

    fn try_get_page(&self, pn: PageNumber) -> Option<(PageRef, bool)> {
        assert!(pn >= self.start);
        let off = pn - self.start;
        let shared = self.is_shared();
        Some((
            match &self.backing {
                BackingPages::Nothing => None,
                BackingPages::Single(page_ref) => {
                    assert!(off < page_ref.nr_pages());
                    Some(page_ref.adjust(self.offset + off))
                }
                BackingPages::Many(pv_ref) => pv_ref.lock().try_get_page(self.offset + off),
            }?,
            shared,
        ))
    }

    fn add_page(&mut self, pn: PageNumber, page: PageRef) -> PageRef {
        assert!(pn >= self.start);
        assert!(pn < self.start.offset(self.length));
        let off = pn - self.start;
        let max = self.start.offset(self.length) - pn;
        let count = max.min(page.nr_pages());
        match &self.backing {
            BackingPages::Nothing => {
                self.backing = BackingPages::Single(page.clone());
                page
            }
            BackingPages::Single(page_ref) => {
                let mut pv = PageVec::new();
                pv.add_page(0, page_ref.clone());
                let r = pv.add_page(off, page.trimmed(count));
                self.backing = BackingPages::Many(Arc::new(Mutex::new(pv)));
                self.offset = 0;
                r
            }
            BackingPages::Many(pv_ref) => pv_ref.lock().add_page(self.offset + off, page),
        }
    }

    pub fn pv_ref_count(&self) -> usize {
        match &self.backing {
            BackingPages::Nothing => 1,
            BackingPages::Single(page_ref) => page_ref.ref_count(),
            BackingPages::Many(pv_ref) => Arc::strong_count(pv_ref),
        }
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

        match &self.backing {
            BackingPages::Many(pv) => pv.lock().truncate_and_drain(self.offset, self.length),
            _ => {}
        }
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

    pub fn range(&self) -> core::ops::Range<PageNumber> {
        self.start..self.start.offset(self.length)
    }

    pub fn sleeper(&mut self) -> Arc<RangeSleep> {
        if self.sleep.is_none() {
            self.sleep = Some(Arc::new(RangeSleep::new()));
        }
        self.sleep.as_ref().cloned().unwrap()
    }

    pub fn is_locked(&self) -> bool {
        self.sleep.as_ref().is_some_and(|s| *s.locked.lock())
    }
}

impl Display for PageRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let bs = match &self.backing {
            BackingPages::Nothing => "empty".to_owned(),
            BackingPages::Single(page_ref) => alloc::format!(
                "{:x}:{}:{}",
                page_ref.physical_address(),
                page_ref.page_offset(),
                page_ref.nr_pages()
            ),
            BackingPages::Many(pv) => pv.lock().show_part(self),
        };
        write!(
            f,
            "{{{}, {}, {}}} -> {} {}",
            self.start,
            self.length,
            self.offset,
            if self.is_shared() { "s" } else { "p" },
            bs
        )
    }
}

#[derive(Default)]
pub struct PageRangeTree {
    tree: NonOverlappingIntervalTree<PageNumber, PageRange>,
    id: ObjID,
}

pub enum PageStatus {
    Ready(PageRef, bool),
    NoPage,
    AllocFail,
    DataFail,
    Locked(Arc<RangeSleep>),
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct GetPageFlags : u32 {
        const WRITE = 1;
        const STABLE = 2;
    }
}

impl PageRangeTree {
    pub fn new(id: ObjID) -> Self {
        Self {
            tree: NonOverlappingIntervalTree::new(),
            id,
        }
    }

    pub fn clear(&mut self) {
        self.tree.clear();
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
        let backing = if discard {
            BackingPages::Nothing
        } else {
            let mut clone = |backing: &BackingPages| -> Option<BackingPages> {
                Some(match backing {
                    BackingPages::Nothing => BackingPages::Nothing,
                    BackingPages::Single(page_ref) => {
                        let new_page = Arc::new(Page::new(allocator.try_allocate()?));
                        let mut new_page = PageRef::new(new_page, 0, page_ref.nr_pages());
                        new_page.copy_from(&page_ref);
                        BackingPages::Single(new_page)
                    }
                    BackingPages::Many(pv) => BackingPages::Many(Arc::new(Mutex::new(
                        pv.lock()
                            .clone_pages_limited(r2.offset, r2.length, allocator)?,
                    ))),
                })
            };

            let Some(backing) = clone(&r2.backing) else {
                // Failed to allocate pages, restore the tree.
                self.tree.insert(range.range(), range);
                return false;
            };
            backing
        };

        r2.backing = backing;

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

    fn try_do_get_page(&self, pn: PageNumber) -> Option<(PageRef, bool, bool)> {
        let range = self.get(pn)?;
        let (page, shared) = range.try_get_page(pn)?;
        Some((page, shared, range.is_locked()))
    }

    pub fn get_page(
        &mut self,
        pn: PageNumber,
        flags: GetPageFlags,
        allocator: Option<&mut FrameAllocator>,
    ) -> PageStatus {
        let Some((page, shared, locked)) = self.try_do_get_page(pn) else {
            return PageStatus::NoPage;
        };
        if locked && flags.contains(GetPageFlags::STABLE) {
            let range = self.get_mut(pn).unwrap();
            return PageStatus::Locked(range.sleeper());
        }
        if !shared || !flags.contains(GetPageFlags::WRITE) {
            return PageStatus::Ready(page, shared);
        }
        if let Some(allocator) = allocator {
            log::debug!("split into three: {} {:?}", pn, flags);
            if !self.split_into_three(pn, false, allocator) {
                return PageStatus::AllocFail;
            }
        }
        let Some((page, shared, _)) = self.try_do_get_page(pn) else {
            return PageStatus::NoPage;
        };
        assert!(!shared);
        PageStatus::Ready(page, shared)
    }

    pub fn try_get_page(&mut self, pn: PageNumber, flags: GetPageFlags) -> PageStatus {
        let Some((page, shared, locked)) = self.try_do_get_page(pn) else {
            return PageStatus::NoPage;
        };
        if locked && flags.contains(GetPageFlags::STABLE) {
            let range = self.get_mut(pn).unwrap();
            return PageStatus::Locked(range.sleeper());
        }
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
        page: PageRef,
        allocator: Option<&mut FrameAllocator>,
    ) -> Option<PageRef> {
        const MAX_EXTENSION_ALLOWED: usize = 16;
        let range = self.tree.get(&pn);
        if let Some(range) = range {
            if range.is_shared() {
                if let Some(allocator) = allocator {
                    if !self.split_into_three(pn, true, allocator) {
                        return None;
                    }
                }
            }
            let mut range = self.tree.remove(&pn).unwrap();
            let off = pn - range.start;
            let extra_len = (page.nr_pages() + off).saturating_sub(range.length);
            range.length += extra_len;
            let p = range.add_page(pn, page);
            let _kicked = self.tree.insert_replace(range.range(), range);
            Some(p)
        } else {
            // Try to extend a previous range.
            if let Some((_, prev_range)) =
                self.tree.range_mut(PageNumber::from_offset(0)..pn).last()
            {
                let end = prev_range.start.offset(prev_range.length - 1);
                let nr_extra_pages = page.nr_pages() - 1;
                let diff = pn - end;
                if !prev_range.is_shared() && diff <= MAX_EXTENSION_ALLOWED {
                    let mut prev_range = self.tree.remove(&end).unwrap();
                    prev_range.length += diff + nr_extra_pages;
                    let p = prev_range.add_page(pn, page);

                    let kicked = self.tree.insert_replace(prev_range.range(), prev_range);
                    assert_eq!(kicked.len(), 0);
                    return Some(p);
                }
            }
            let mut range = PageRange::new(pn);
            range.length = page.nr_pages();
            let p = range.add_page(pn, page);
            let kicked = self.tree.insert_replace(range.range(), range);
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
