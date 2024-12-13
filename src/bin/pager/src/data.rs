use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use bitvec::prelude::*;
use twizzler_abi::pager::{ObjectRange, PhysRange};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};
use std::collections::VecDeque;

#[derive(Clone)]
pub struct PagerData {
    inner: Arc<Mutex<PagerDataInner>>,
}

pub struct PagerDataInner {
    pub bitvec: BitVec,
    pub hashmap: HashMap<u64, (ObjID, ObjectRange)>,
    pub lru_queue: VecDeque<u64>,
    pub mem_range_start: u64 
}

impl PagerDataInner {
    /// Create a new PagerDataInner instance
    pub fn new() -> Self {
        println!("[pager] initializing PagerDataInner");
        PagerDataInner {
            bitvec: BitVec::new(),
            hashmap: HashMap::with_capacity(0),
            lru_queue: VecDeque::new(),
            mem_range_start: 0
        }
    }

    pub fn set_range_start(&mut self, start: u64) {
        self.mem_range_start = start;
    }

    /// Get the next available page number and insert it into the bitvec.
    /// Returns `Some(page_number)` if a page is available, or `None` if none are left.
    fn get_next_available_page(&mut self) -> Option<usize> {
        println!("[pager] searching for next available page");
        let next_page = self.bitvec.iter().position(|bit| !bit);

        if let Some(page_number) = next_page {
            self.bitvec.set(page_number, true);
            self.lru_queue.push_back(page_number.try_into().unwrap());
            println!("[pager] next available page: {}", page_number);
            Some(page_number)
        } else {
            println!("[pager] no available pages left");
            None
        }
    }

    /// Page replacement algorithm (LRU strategy)
    fn page_replacement(&mut self) -> u64 {
        println!("[pager] executing page replacement");
        if let Some(old_page) = self.lru_queue.pop_front() {
            println!("[pager] replacing page: {}", old_page);
            self.remove_page(old_page as usize);
            old_page
        } else {
            panic!("[pager] page replacement failed: no pages to replace");
        }
    }
    

    fn get_mem_page(&mut self) -> usize {
        println!("[pager] attempting to get memory page");
        if self.bitvec.all() {
            println!("[pager] all pages used, initiating page replacement");
            self.page_replacement();
        }
        self.get_next_available_page().expect("[pager] no available pages")
    }

    /// Remove a page from the bitvec.
    fn remove_page(&mut self, page_number: usize) {
        println!("[pager] attempting to remove page {}", page_number);
        if page_number < self.bitvec.len() {
            self.bitvec.set(page_number, false);
            self.remove_from_map(&(page_number as u64));
            self.lru_queue.retain(|&p| p != page_number as u64);
            println!("[pager] page {} removed from bitvec", page_number);
        } else {
            println!("[pager] page {} is out of bounds and cannot be removed", page_number);
        }
    }

    /// Adjust the size of the bitvec dynamically.
    fn resize_bitset(&mut self, new_size: usize) {
        println!("[pager] resizing bitvec to new size: {}", new_size);
        if new_size == 0 {
            println!("[pager] clearing bitvec");
            self.bitvec.clear();
        } else {
            self.bitvec.resize(new_size, false);
        }
        println!("[pager] bitvec resized to: {}", new_size);
    }

    pub fn is_full(&self) -> bool {
        let full = self.bitvec.all();
        println!("[pager] bitvec check full: {}", full);
        full
    }

    pub fn insert_into_map(&mut self, key: u64, obj_id: ObjID, range: ObjectRange) {
        println!(
            "[pager] inserting into hashmap: key = {}, ObjID = {:?}, ObjectRange = {:?}",
            key, obj_id, range
        );
        self.hashmap.insert(key, (obj_id.clone(), range.clone()));
        println!(
            "[pager] inserted into hashmap: key = {}, ObjID = {:?}, ObjectRange = {:?}",
            key, obj_id, range
        );
    }
    
    pub fn update_on_key(&mut self, key: u64) {
        self.lru_queue.retain(|&p| p != key);
        self.lru_queue.push_back(key);
    }

    pub fn get_from_map(&mut self, key: &u64) -> Option<(ObjID, ObjectRange)> {
        println!("[pager] retrieving value for key {}", key);
        match self.hashmap.get(key) {
            Some(value) => {
                println!("[pager] value found for key {}: {:?}", key, value);
                self.lru_queue.retain(|&p| p != *key);
                self.lru_queue.push_back(*key);
                Some(value.clone())
            }
            None => {
                println!("[pager] no value found for key: {}", key);
                None
            }
        }
    }

    pub fn remove_from_map(&mut self, key: &u64) {
        println!("[pager] removing key {} from hashmap", key);
        self.hashmap.remove(key);
    }

    pub fn resize_map(&mut self, add_size: usize) {
        println!("[pager] adding {} capacity to hashmap", add_size);
        self.hashmap.reserve(add_size);
    }
}

impl PagerData {
    /// Create a new PagerData instance
    pub fn new() -> Self {
        println!("[pager] creating new PagerData instance");
        PagerData {
            inner: Arc::new(Mutex::new(PagerDataInner::new())),
        }
    }

    /// Map + Bitset Operations
    pub fn resize(&self, pages: usize) {
        println!("[pager] resizing resources to support {} pages", pages);
        let mut inner = self.inner.lock().unwrap();
        inner.resize_bitset(pages);
        inner.resize_map(pages);
    }

    pub fn init_range(&self, range: PhysRange) {
        self.inner.lock().unwrap().set_range_start(range.start);
    }

    pub fn alloc_mem_page(&self, id: ObjID, range: ObjectRange) -> usize {
        println!("[pager] allocating memory page for ObjID {:?}, ObjectRange {:?}", id, range);
        let mut inner = self.inner.lock().unwrap();
        let page = inner.get_mem_page();
        inner.insert_into_map(page.try_into().unwrap(), id, range);
        println!("[pager] memory page allocated successfully");
        return page;
    }

    pub fn test_alloc_page(&self) {
        let mut inner = self.inner.lock().unwrap();
        while !inner.is_full() {
            let page = inner.get_mem_page();
        }
        for i in 0..90 {
            inner.update_on_key(i);
        }
    }
}

