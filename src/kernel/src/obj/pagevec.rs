use alloc::{format, string::String, sync::Arc, vec::Vec};

use crate::mutex::Mutex;

use super::{
    pages::{Page, PageRef},
    range::PageRange,
};

pub struct PageVec {
    pages: Vec<Option<PageRef>>,
}

pub type PageVecRef = Arc<Mutex<PageVec>>;

impl PageVec {
    pub fn new() -> Self {
        Self {
            pages: alloc::vec![],
        }
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Remove the first elements up to offset, and then truncate the vector to the given length.
    pub fn truncate_and_drain(&mut self, offset: usize, len: usize) {
        self.pages.drain(0..offset);
        self.pages.truncate(len);
        if self.pages.capacity() > 2 * len {
            self.pages.shrink_to_fit();
        }
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
        for p in self.pages.iter().skip(range.offset).take(range.length) {
            if !first {
                str += ", ";
            }
            if let Some(p) = p {
                str += &format!("{:x}", p.physical_address());
            } else {
                str += "None";
            }
            first = false;
        }

        str += ", ...]";

        str
    }

    pub fn clone_pages(&self) -> Self {
        let mut pv = Self::new();
        for (i, p) in self.pages.iter().enumerate() {
            if let Some(page) = p {
                pv.pages.resize(i + 1, None);
                pv.pages[i] = Some(Arc::new(page.copy_page()));
            }
        }
        pv
    }

    pub fn clone_pages_limited(&self, start: usize, len: usize) -> Self {
        let mut pv = Self::new();
        for (di, si) in (start..(start + len)).enumerate() {
            if let Some(page) = &self.pages[si] {
                pv.pages.resize(di + 1, None);
                pv.pages[di] = Some(Arc::new(page.copy_page()));
            }
        }
        pv
    }

    pub fn get_page(&mut self, offset: usize) -> PageRef {
        if offset >= self.pages.len() {
            self.pages.resize(offset + 1, None)
        }
        if let Some(ref page) = self.pages[offset] {
            page.clone()
        } else {
            self.pages[offset] = Some(Arc::new(Page::new()));
            self.pages[offset].as_ref().unwrap().clone()
        }
    }

    pub fn add_page(&mut self, offset: usize, page: Page) {
        if offset >= self.pages.len() {
            self.pages.reserve((offset + 1) * 2);
            self.pages.resize(offset + 1, None)
        }
        self.pages[offset] = Some(Arc::new(page));
    }
}
