use alloc::{format, string::String, sync::Arc};
use core::{ops::Range, usize};

use nonoverlapping_interval_tree::NonOverlappingIntervalTree;

use super::{
    pages::{Page, PageRef},
    range::PageRange,
};
use crate::{
    memory::{pagetables::MappingSettings, tracker::FrameAllocator},
    mutex::Mutex,
};

#[derive(Debug)]
pub struct PageVec {
    tree: NonOverlappingIntervalTree<usize, PageRef>,
}

pub type PageVecRef = Arc<Mutex<PageVec>>;

impl PageVec {
    pub fn new() -> Self {
        Self {
            tree: NonOverlappingIntervalTree::new(),
        }
    }

    pub fn first(&self) -> Option<&PageRef> {
        self.tree
            .range(Range {
                start: 0,
                end: usize::MAX,
            })
            .next()
            .map(|x| x.1.value())
    }

    pub fn len(&self) -> usize {
        self.tree.len()
    }

    /// Remove the first pages up to offset, and then truncate the vector to the given page count.
    pub fn truncate_and_drain(&mut self, _offset: usize, _pages: usize) {
        logln!("todo: truncate and drain");
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
        for (k, entry) in self.tree.range(range.offset..(range.offset + range.length)) {
            if !first {
                str += ", ";
            }
            str += &format!("{}:{:x}", k, entry.physical_address());
            first = false;
        }
        str += ", ...]";

        str
    }

    pub fn show_entry(&self, offset: usize, length: usize) -> String {
        let mut str = String::new();
        str += &format!("PV {:p} ", self);
        if offset > 0 {
            str += "[..., ";
        } else {
            str += "[";
        }

        let mut first = true;
        for (k, entry) in self.tree.range(offset..(offset + length)) {
            if !first {
                str += ", ";
            }
            str += &format!("{}:{:x}", k, entry.physical_address());
            first = false;
        }
        str += ", ...]";

        str
    }

    pub fn clone_pages_limited(
        &self,
        start: usize,
        len: usize,
        allocator: &mut FrameAllocator,
    ) -> Option<Self> {
        let mut pv = Self::new();
        let range = self.tree.range(start..(start + len));

        for (k, entry) in range {
            let thisrange = (*k)..(*entry.end());
            // TODO: use larger pages
            for i in 0..entry.nr_pages() {
                let new_page = Arc::new(Page::new(allocator.try_allocate()?));
                let mut new_page = PageRef::new(new_page, 0, 1);
                new_page.copy_from(&entry.adjust(i));
                pv.tree.insert(thisrange.clone(), new_page);
            }
        }

        Some(pv)
    }

    pub fn try_get_page(&self, pn: usize) -> Option<PageRef> {
        let mut entry = self.tree.range(pn..(pn + 1));
        let entry = entry.next()?;
        Some(entry.1.adjust(pn - *entry.0))
    }

    pub fn pages<const MAX: usize>(
        &self,
        pn: usize,
        pages: &mut heapless::Vec<(PageRef, MappingSettings), MAX>,
        settings: MappingSettings,
    ) {
        let entry = self.tree.range(pn..(pn + pages.capacity()));

        let mut start = pn;
        for entry in entry {
            if *entry.0 == start && !pages.is_full() {
                unsafe {
                    pages.push_unchecked((entry.1.value().clone(), settings));
                }
                start += entry.1.value().nr_pages();
            } else {
                break;
            }
        }
    }

    pub fn add_page(&mut self, off: usize, page: PageRef) -> PageRef {
        let range = off..(off + page.nr_pages());
        let _k = self.tree.insert_replace(range.clone(), page.clone());
        page
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

    fn new_page() -> PageRef {
        PageRef::new(
            Arc::new(Page::new(alloc_frame(FrameAllocFlags::empty()))),
            0,
            1,
        )
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

        assert_eq!(x.as_virtaddr(), x_addr);
        assert_eq!(y.as_virtaddr(), y_addr);
        assert_eq!(z.as_virtaddr(), z_addr);

        let xr = pv.try_get_page(0).unwrap();
        let yr = pv.try_get_page(100).unwrap();
        let zr = pv.try_get_page(50).unwrap();

        assert_eq!(xr.as_virtaddr(), x_addr);
        assert_eq!(yr.as_virtaddr(), y_addr);
        assert_eq!(zr.as_virtaddr(), z_addr);
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

        assert_eq!(x.as_virtaddr(), x_addr);
        assert_eq!(y.as_virtaddr(), y_addr);
        assert_eq!(z.as_virtaddr(), z_addr);
        assert_eq!(w.as_virtaddr(), w_addr);

        let xr = pv.try_get_page(0).unwrap();
        let wr = pv.try_get_page(1).unwrap();
        let zr = pv.try_get_page(2).unwrap();

        assert_eq!(xr.as_virtaddr(), x_addr);
        assert_eq!(zr.as_virtaddr(), z_addr);
        assert_eq!(wr.as_virtaddr(), w_addr);
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

            for pn in 0..1000 {
                let testval = pv.try_get_page(pn);
                match testval {
                    Some(v) => {
                        assert_eq!(v.as_virtaddr(), *truth.get(&pn).unwrap())
                    }
                    None => {
                        assert!(truth.get(&pn).is_none());
                    }
                }
            }
        }
    }
}
