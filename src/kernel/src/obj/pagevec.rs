use alloc::{sync::Arc, vec::Vec};

use crate::mutex::Mutex;

use super::pages::PageRef;

pub struct PageVec {
    pages: Vec<PageRef>,
}

pub type PageVecRef = Arc<Mutex<PageVec>>;

impl PageVec {
    pub fn get_page(&self, offset: usize) -> PageRef {
        self.pages[offset].clone()
    }
}
