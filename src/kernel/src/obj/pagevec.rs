use alloc::{format, string::String, sync::Arc, vec::Vec};
use core::num::NonZeroUsize;

use super::{
    pages::{Page, PageRef},
    range::PageRange,
    PageNumber,
};
use crate::{memory::tracker::FrameAllocator, mutex::Mutex};

enum PageOrHole {
    Hole(NonZeroUsize),
    Page(PageRef, usize),
}

impl PageOrHole {
    pub fn nr_pages(&self) -> usize {
        match self {
            PageOrHole::Hole(count) => count.get(),
            PageOrHole::Page(page, off) => page.nr_pages() - off,
        }
    }
}

pub struct PageVec {
    todo: replace with interval tree.
    pages: Vec<PageOrHole>,
    idx: Vec<(u32, u32)>,
    num_pages: usize,
}

pub type PageVecRef = Arc<Mutex<PageVec>>;

impl PageVec {
    pub fn new() -> Self {
        Self {
            pages: alloc::vec![],
            idx: alloc::vec![],
            num_pages: 0,
        }
    }

    /// Remove the first pages up to offset, and then truncate the vector to the given page count.
    /// Returns the new offset.
    #[must_use]
    pub fn truncate_and_drain(&mut self, mut offset: usize, mut pages: usize) -> usize {
        let Some(new_zero) = self.pages.iter().position(|e| {
            if offset == 0 {
                true
            } else {
                let this_count = e.nr_pages();
                if offset >= this_count {
                    offset -= this_count;
                    false
                } else {
                    true
                }
            }
        }) else {
            return offset;
        };

        for e in self.pages.drain(0..new_zero) {
            self.num_pages -= e.nr_pages();
        }

        let Some(new_last) = self.pages.iter().position(|e| {
            if pages == 0 {
                true
            } else {
                let this_count = e.nr_pages();
                if pages >= this_count {
                    pages -= this_count;
                    false
                } else {
                    true
                }
            }
        }) else {
            self.rebuild_index();
            return offset;
        };

        for e in self.pages.drain((new_last + 1)..) {
            self.num_pages -= e.nr_pages();
        }

        if self.pages.capacity() > 2 * self.pages.len() {
            self.pages.shrink_to_fit();
        }
        self.rebuild_index();
        offset
    }

    fn rebuild_index(&mut self) {
        let mut new_index = Vec::new();
        for (i, entry) in self.pages.iter().enumerate() {
            new_index.extend(
                (0..entry.nr_pages())
                    .into_iter()
                    .map(|p| (i as u32, p as u32)),
            );
        }
        self.idx = new_index;
    }

    fn find_start(&self, pn: usize) -> Option<(usize, usize)> {
        self.idx.get(pn).map(|(a, b)| (*a as usize, *b as usize))
    }

    fn find_end(&self, pn: usize) -> usize {
        self.idx
            .get(pn)
            .map(|(a, _)| *a as usize)
            .unwrap_or(self.pages.len())
    }

    pub fn show_part(&self, range: &PageRange) -> String {
        let mut str = String::new();
        str += &format!("PV {:p} ", self);
        if range.offset > 0 {
            str += "[..., ";
        } else {
            str += "[";
        }

        let mut first = true;
        if let Some((off_idx, _)) = self.find_start(range.offset) {
            let (end_idx, _) = self
                .find_start(range.offset + range.length)
                .unwrap_or((usize::MAX, 0));
            for p in self.pages.iter().skip(off_idx).take(end_idx - off_idx) {
                if !first {
                    str += ", ";
                }
                match p {
                    PageOrHole::Hole(count) => {
                        str += &format!("{} hole", count);
                    }
                    PageOrHole::Page(page, off) => {
                        str += &format!(
                            "{:x}",
                            page.physical_address()
                                .offset(off * PageNumber::PAGE_SIZE)
                                .unwrap()
                        );
                    }
                }
                first = false;
            }
        }
        str += ", ...]";

        str
    }

    pub fn clone_pages_limited(
        &self,
        start: usize,
        len: usize,
        allocator: &mut FrameAllocator,
    ) -> Option<(Self, usize)> {
        let mut pv = Self::new();
        let (start, off) = self.find_start(start).unwrap();
        let end = self.find_end(start + len);
        for si in start..end {
            match &self.pages[si] {
                PageOrHole::Hole(non_zero) => pv.pages.push(PageOrHole::Hole(*non_zero)),
                PageOrHole::Page(page, off) => {
                    let frame = allocator.try_allocate()?;
                    pv.pages.push(PageOrHole::Page(
                        Arc::new(page.copy_page(
                            frame,
                            page.cache_type(),
                            off * PageNumber::PAGE_SIZE,
                        )),
                        0,
                    ));
                }
            }
        }
        pv.num_pages = pv.pages.iter().fold(0, |acc, e| acc + e.nr_pages());
        pv.rebuild_index();
        Some((pv, off))
    }

    pub fn try_get_page(&self, pn: usize) -> Option<(PageRef, usize)> {
        let (idx, off) = self.find_start(pn)?;
        if let PageOrHole::Page(ref page, poff) = self.pages[idx] {
            Some((page.clone(), off + poff))
        } else {
            None
        }
    }

    fn extend_to(&mut self, pn: usize, page: Page) -> (Arc<Page>, usize) {
        if let Some(hole_len) = NonZeroUsize::new(pn - self.num_pages) {
            self.idx.extend(
                (0..hole_len.get())
                    .into_iter()
                    .map(|p| (self.pages.len() as u32, p as u32)),
            );
            self.pages.push(PageOrHole::Hole(hole_len));
            self.num_pages += hole_len.get();
        }
        self.idx.extend(
            (0..page.nr_pages())
                .into_iter()
                .map(|p| (self.pages.len() as u32, p as u32)),
        );
        let page = Arc::new(page);
        self.pages.push(PageOrHole::Page(page.clone(), 0));
        self.num_pages += page.nr_pages();
        (page, 0)
    }

    pub fn add_page(&mut self, pn: usize, page: Page) -> (Arc<Page>, usize) {
        let Some((mut idx, off)) = self.find_start(pn) else {
            return self.extend_to(pn, page);
        };

        let pages_at_idx = self.pages[idx].nr_pages();
        assert!(off < pages_at_idx);
        if off > 0 {
            match self.pages.remove(idx) {
                PageOrHole::Hole(cur) => {
                    let split_count = cur.get() - off;
                    self.pages.insert(
                        idx,
                        PageOrHole::Hole(NonZeroUsize::new(split_count).unwrap()),
                    );
                    self.pages
                        .insert(idx, PageOrHole::Hole(NonZeroUsize::new(off).unwrap()));
                    idx += 1;
                }
                PageOrHole::Page(oldpage, poff) => {
                    let new_pages = (0..oldpage.nr_pages())
                        .into_iter()
                        .map(|offpn| (oldpage.clone(), poff + offpn))
                        .collect::<Vec<_>>();
                    // TODO: this sucks
                    for p in new_pages.into_iter().rev() {
                        self.pages.insert(idx, PageOrHole::Page(p.0, p.1));
                    }
                    idx += off;
                }
            }
        }

        let mut rem_count = page.nr_pages();
        while self.pages.len() > idx && rem_count >= self.pages[idx].nr_pages() {
            let e = self.pages.remove(idx);
            rem_count -= e.nr_pages();
        }
        if rem_count > 0 && self.pages.len() > idx {
            match self.pages.remove(idx) {
                PageOrHole::Hole(cur) => {
                    let split_count = cur.get() - rem_count;
                    self.pages.insert(
                        idx,
                        PageOrHole::Hole(NonZeroUsize::new(split_count).unwrap()),
                    );
                }
                PageOrHole::Page(oldpage, poff) => {
                    let new_pages = (0..oldpage.nr_pages())
                        .into_iter()
                        .map(|offpn| (oldpage.clone(), poff + offpn))
                        .collect::<Vec<_>>();
                    // TODO: this sucks
                    for p in new_pages.into_iter().rev().skip(rem_count) {
                        self.pages.insert(idx, PageOrHole::Page(p.0, p.1));
                    }
                }
            }
        }

        let page = Arc::new(page);
        self.pages.insert(idx, PageOrHole::Page(page.clone(), 0));
        self.rebuild_index();
        (page, 0)
    }
}

mod tests {
    use alloc::collections::btree_map::BTreeMap;

    use twizzler_kernel_macros::kernel_test;

    use super::*;
    use crate::{
        memory::tracker::{alloc_frame, FrameAllocFlags},
        utils::quick_random,
    };

    fn new_page() -> Page {
        Page::new(alloc_frame(FrameAllocFlags::empty()))
    }

    #[kernel_test]
    fn test_pagevec() {
        let mut pv = PageVec::new();
        let ox = new_page();
        let oy = new_page();
        let oz = new_page();

        let x_addr = ox.as_virtaddr();
        let y_addr = oy.as_virtaddr();
        let z_addr = oz.as_virtaddr();

        let x = pv.add_page(0, ox);
        let y = pv.add_page(100, oy);
        let z = pv.add_page(50, oz);

        assert_eq!(x.0.as_virtaddr(), x_addr);
        assert_eq!(y.0.as_virtaddr(), y_addr);
        assert_eq!(z.0.as_virtaddr(), z_addr);

        let xr = pv.try_get_page(0).unwrap();
        let yr = pv.try_get_page(100).unwrap();
        let zr = pv.try_get_page(50).unwrap();

        assert_eq!(xr.0.as_virtaddr(), x_addr);
        assert_eq!(yr.0.as_virtaddr(), y_addr);
        assert_eq!(zr.0.as_virtaddr(), z_addr);
    }

    #[kernel_test]
    fn test_pagevec_replace() {
        let mut pv = PageVec::new();
        let ox = new_page();
        let oy = new_page();
        let oz = new_page();
        let ow = new_page();

        let x_addr = ox.as_virtaddr();
        let y_addr = oy.as_virtaddr();
        let z_addr = oz.as_virtaddr();
        let w_addr = ow.as_virtaddr();

        let x = pv.add_page(0, ox);
        let y = pv.add_page(1, oy);
        let z = pv.add_page(2, oz);
        let w = pv.add_page(1, ow);

        assert_eq!(x.0.as_virtaddr(), x_addr);
        assert_eq!(y.0.as_virtaddr(), y_addr);
        assert_eq!(z.0.as_virtaddr(), z_addr);
        assert_eq!(w.0.as_virtaddr(), w_addr);

        let xr = pv.try_get_page(0).unwrap();
        let wr = pv.try_get_page(1).unwrap();
        let zr = pv.try_get_page(2).unwrap();

        assert_eq!(xr.0.as_virtaddr(), x_addr);
        assert_eq!(zr.0.as_virtaddr(), z_addr);
        assert_eq!(wr.0.as_virtaddr(), w_addr);
    }

    #[kernel_test]
    fn test_pagevec_fuzz() {
        let mut truth = BTreeMap::new();
        let mut pv = PageVec::new();
        for i in 1..1000 {
            let p = quick_random() as usize % i;
            let page = new_page();
            truth.insert(p, page.as_virtaddr());
            pv.add_page(p as usize, page);

            for pn in 0..pv.num_pages {
                let testval = pv.try_get_page(pn);
                match testval {
                    Some(v) => {
                        assert_eq!(v.0.as_virtaddr(), *truth.get(&pn).unwrap())
                    }
                    None => {
                        assert!(truth.get(&pn).is_none());
                    }
                }
            }
        }
    }
}
