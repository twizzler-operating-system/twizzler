use alloc::{sync::Arc, vec::Vec};

use crate::mutex::Mutex;

use super::pages::{Page, PageRef};

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
            self.pages.resize(offset + 1, None)
        }
        assert!(self.pages[offset].is_none()); //TODO
        self.pages[offset] = Some(Arc::new(page));
    }
}
