use std::sync::{Arc, Mutex};
use bit_set::BitSet;

#[derive(Clone)]
pub struct PagerData {
    pub bitset: Arc<Mutex<BitSet>>,
}

impl PagerData {
    /// Create a new PagerData instance with the given size
    /// The size is the total number of bits needed.
    pub fn new() -> Self {
        PagerData {
            bitset: Arc::new(Mutex::new(BitSet::new())),
        }
    }

    /// Adjust the size of the bitmap dynamically.
    pub fn resize_bitset(&self, new_size: usize) {
        let mut bitset = self.bitset.lock().unwrap();

        if new_size == 0 {
            bitset.clear();
        } else {
            bitset.reserve_len(new_size - 1);
        }

    }
}
