use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use bitvec::prelude::*;
use twizzler_abi::pager::{ObjectRange, PhysRange};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};
use std::collections::VecDeque;

use crate::helpers::{page_in, page_to_physrange};

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
    /// Initializes the data structure for managing page allocations and replacements.
    pub fn new() -> Self {
        tracing::info!("initializing PagerDataInner");
        PagerDataInner {
            bitvec: BitVec::new(),
            hashmap: HashMap::with_capacity(0),
            lru_queue: VecDeque::new(),
            mem_range_start: 0
        }
    }

    /// Set the starting address of the memory range to be managed.
    pub fn set_range_start(&mut self, start: u64) {
        self.mem_range_start = start;
    }

    /// Get the next available page number and mark it as used.
    /// Returns the page number if available, or `None` if all pages are used.
    fn get_next_available_page(&mut self) -> Option<usize> {
        tracing::info!("searching for next available page");
        let next_page = self.bitvec.iter().position(|bit| !bit);

        if let Some(page_number) = next_page {
            self.bitvec.set(page_number, true);
            self.lru_queue.push_back(page_number.try_into().unwrap());
            tracing::info!("next available page: {}", page_number);
            Some(page_number)
        } else {
            tracing::info!("no available pages left");
            None
        }
    }

    /// Perform page replacement using the Least Recently Used (LRU) strategy.
    /// Returns the identifier of the replaced page.
    fn page_replacement(&mut self) -> u64 {
        tracing::info!("executing page replacement");
        if let Some(old_page) = self.lru_queue.pop_front() {
            tracing::info!("replacing page: {}", old_page);
            self.remove_page(old_page as usize);
            old_page
        } else {
            panic!("page replacement failed: no pages to replace");
        }
    }

    /// Get a memory page for allocation.
    /// Triggers page replacement if all pages are used.
    fn get_mem_page(&mut self) -> usize {
        tracing::info!("attempting to get memory page");
        if self.bitvec.all() {
            tracing::info!("all pages used, initiating page replacement");
            self.page_replacement();
        }
        self.get_next_available_page().expect("no available pages")
    }

    /// Remove a page from the bit vector, freeing it for future use.
    fn remove_page(&mut self, page_number: usize) {
        tracing::info!("attempting to remove page {}", page_number);
        if page_number < self.bitvec.len() {
            self.bitvec.set(page_number, false);
            self.remove_from_map(&(page_number as u64));
            self.lru_queue.retain(|&p| p != page_number as u64);
            tracing::info!("page {} removed from bitvec", page_number);
        } else {
            tracing::info!("page {} is out of bounds and cannot be removed", page_number);
        }
    }

    /// Resize the bit vector to accommodate more pages or clear it.
    fn resize_bitset(&mut self, new_size: usize) {
        tracing::info!("resizing bitvec to new size: {}", new_size);
        if new_size == 0 {
            tracing::info!("clearing bitvec");
            self.bitvec.clear();
        } else {
            self.bitvec.resize(new_size, false);
        }
        tracing::info!("bitvec resized to: {}", new_size);
    }

    /// Check if all pages are currently in use.
    pub fn is_full(&self) -> bool {
        let full = self.bitvec.all();
        tracing::info!("bitvec check full: {}", full);
        full
    }

    /// Insert an object and its associated range into the hashmap.
    pub fn insert_into_map(&mut self, key: u64, obj_id: ObjID, range: ObjectRange) {
        tracing::info!(
            "inserting into hashmap: key = {}, ObjID = {:?}, ObjectRange = {:?}",
            key, obj_id, range
        );
        self.hashmap.insert(key, (obj_id.clone(), range.clone()));
        tracing::info!(
            "inserted into hashmap: key = {}, ObjID = {:?}, ObjectRange = {:?}",
            key, obj_id, range
        );
    }

    /// Update the LRU queue based on access to a key.
    pub fn update_on_key(&mut self, key: u64) {
        self.lru_queue.retain(|&p| p != key);
        self.lru_queue.push_back(key);
    }

    /// Retrieve an object and its range from the hashmap by key.
    /// Updates the LRU queue to reflect access.
    pub fn get_from_map(&mut self, key: &u64) -> Option<(ObjID, ObjectRange)> {
        tracing::info!("retrieving value for key {}", key);
        match self.hashmap.get(key) {
            Some(value) => {
                tracing::info!("value found for key {}: {:?}", key, value);
                self.lru_queue.retain(|&p| p != *key);
                self.lru_queue.push_back(*key);
                Some(value.clone())
            }
            None => {
                tracing::info!("no value found for key: {}", key);
                None
            }
        }
    }

    /// Remove a key and its associated value from the hashmap.
    pub fn remove_from_map(&mut self, key: &u64) {
        tracing::info!("removing key {} from hashmap", key);
        self.hashmap.remove(key);
    }

    /// Reserve additional capacity in the hashmap.
    pub fn resize_map(&mut self, add_size: usize) {
        tracing::info!("adding {} capacity to hashmap", add_size);
        self.hashmap.reserve(add_size);
    }
}

impl PagerData {
    /// Create a new PagerData instance.
    /// Wraps PagerDataInner with thread-safe access.
    pub fn new() -> Self {
        tracing::info!("creating new PagerData instance");
        PagerData {
            inner: Arc::new(Mutex::new(PagerDataInner::new())),
        }
    }

    /// Resize the internal structures to accommodate the given number of pages.
    pub fn resize(&self, pages: usize) {
        tracing::info!("resizing resources to support {} pages", pages);
        let mut inner = self.inner.lock().unwrap();
        inner.resize_bitset(pages);
        inner.resize_map(pages);
    }

    /// Initialize the starting memory range for the pager.
    pub fn init_range(&self, range: PhysRange) {
        self.inner.lock().unwrap().set_range_start(range.start);
    }

    /// Allocate a memory page and associate it with an object and range.
    /// Page in the data from disk
    /// Returns the physical range corresponding to the allocated page.
    pub fn fill_mem_page(&self, id: ObjID, obj_range: ObjectRange) -> PhysRange {
        tracing::info!("allocating memory page for ObjID {:?}, ObjectRange {:?}", id, obj_range);
        let mut inner = self.inner.lock().unwrap();
        let page = inner.get_mem_page();
        inner.insert_into_map(page.try_into().unwrap(), id, obj_range);
        let phys_range = page_to_physrange(page, 0);
        page_in(id, obj_range, phys_range);
        tracing::info!("memory page allocated successfully");
        return phys_range;
    }
}

