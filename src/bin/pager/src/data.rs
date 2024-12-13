use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use bitvec::prelude::*;
use twizzler_abi::pager::{ObjectRange};
use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

#[derive(Clone)]
pub struct PagerData {
    inner: Arc<Mutex<PagerDataInner>>,
}

pub struct PagerDataInner {
    pub bitvec: BitVec,
    pub hashmap: HashMap<u64, (ObjID, ObjectRange)>,
}

impl PagerDataInner {
    /// Create a new PagerDataInner instance
    pub fn new() -> Self {
        println!("[pager] initializing pagerdatainner");
        PagerDataInner {
            bitvec: BitVec::new(),
            hashmap: HashMap::with_capacity(0),
        }
    }
}

impl PagerData {
    /// Create a new PagerData instance
    pub fn new() -> Self {
        println!("[pager] creating a new pagerdata instance");
        PagerData {
            inner: Arc::new(Mutex::new(PagerDataInner::new())),
        }
    }
    
    /// Map + Bitset Operations
    ///
    pub fn resize(&self, pages: usize) {
        self.resize_bitset(pages);
        self.resize_map(pages);
    }

    /// Bitset Operations


    /// Get the next available page number and insert it into the bitvec.
    /// Returns `Some(page_number)` if a page is available, or `None` if none are left.
    pub fn get_next_available_page(&self) -> Option<usize> {
        println!("[pager] pager lock: acquiring lock to get the next available page");
        let mut inner = self.inner.lock().unwrap();

        // Find the first unset bit
        let next_page = inner.bitvec.iter().position(|bit| !bit);

        if let Some(page_number) = next_page {
            if page_number >= inner.bitvec.len() {
                inner.bitvec.resize(page_number + 1, false);
            }
            inner.bitvec.set(page_number, true);
            println!("[pager] next available page: {}", page_number);
            Some(page_number)
        } else {
            println!("[pager] no available pages left");
            None
        }
    }

    /// Check if the bitvec is full.
    pub fn is_full(&self) -> bool {
        println!("[pager] pager lock: acquiring lock to check if bitvec is full");
        let inner = self.inner.lock().unwrap();
        let full = inner.bitvec.all();
        println!("[pager] bitvec is full: {}", full);
        full
    }

    /// Remove a page from the bitvec.
    pub fn remove_page(&self, page_number: usize) {
        println!("[pager] pager lock: acquiring lock to remove page: {}", page_number);
        let mut inner = self.inner.lock().unwrap();

        if page_number < inner.bitvec.len() {
            inner.bitvec.set(page_number, false);
            println!("[pager] page {} removed from bitvec", page_number);
        } else {
            println!("[pager] page {} is out of bounds and cannot be removed", page_number);
        }
    }

    /// Adjust the size of the bitvec dynamically.
    pub fn resize_bitset(&self, new_size: usize) {
        println!("[pager] pager lock: acquiring lock to resize bitvec");
        let mut inner = self.inner.lock().unwrap();

        if new_size == 0 {
            println!("[pager] clearing bitvec");
            inner.bitvec.clear();
        } else {
            println!("[pager] resizing bitvec to new size: {}", new_size);
            inner.bitvec.resize(new_size, false);
        }

        println!("[pager] bitvec resized to: {}", new_size);
    }

    /// Hashmap Operations
    

    pub fn insert_into_map(&self, key: u64, obj_id: ObjID, range: ObjectRange) {
        println!(
            "[pager] pager lock: acquiring lock to insert into hashmap: key = {}, ObjID = {:?}, ObjectRange = {:?}",
            key, obj_id, range
        );
        let mut inner = self.inner.lock().unwrap();
        inner.hashmap.insert(key, (obj_id.clone(), range.clone()));
        println!(
            "[pager] inserted key-value pair into hashmap: key = {}, ObjID = {:?}, ObjectRange = {:?}",
            key, obj_id, range
        );
    }

    pub fn get_from_map(&self, key: &u64) -> Option<(ObjID, ObjectRange)> {
        println!("[pager] pager lock: acquiring lock to retrieve value for key: {}", key);
        let inner = self.inner.lock().unwrap();
        match inner.hashmap.get(key) {
            Some(value) => {
                println!("[pager] value retrieved for key {}: {:?}", key, value);
                Some(value.clone())
            }
            None => {
                println!("[pager] no value found for key: {}", key);
                None
            }
        }
    }

    pub fn resize_map(&self, add_size: usize) {
        println!("[pager] pager lock: acquiring lock to resize hashmap");
        let mut inner = self.inner.lock().unwrap();
        inner.hashmap.reserve(add_size);
        println!("[pager] additional capacity of {} added to hashmap", add_size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resize_bitset() {
        let pager_data = PagerData::new();
        pager_data.resize_bitset(10);
        assert_eq!(pager_data.inner.lock().unwrap().bitvec.len(), 10);
        pager_data.resize_bitset(0);
        assert!(pager_data.inner.lock().unwrap().bitvec.is_empty());
    }

    #[test]
    fn test_insert_and_retrieve_from_map() {
        let pager_data = PagerData::new();
        let obj_id = ObjID(42);
        let range = ObjectRange { start: 0, end: 100 };
        pager_data.insert_into_map(42, obj_id.clone(), range.clone());
        let retrieved = pager_data.get_from_map(&42);
        assert_eq!(retrieved, Some((obj_id, range)));
    }

    #[test]
    fn test_retrieve_nonexistent_key() {
        let pager_data = PagerData::new();
        let retrieved = pager_data.get_from_map(&999);
        assert_eq!(retrieved, None);
    }
}

