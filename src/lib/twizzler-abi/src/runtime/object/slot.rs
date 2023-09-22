/// Allocate a slot in the address space where we could map a new object.
pub fn global_allocate() -> Option<usize> {
    SLOT_TRACKER.lock().alloc()
}

/// Release a slot for reuse.
#[allow(dead_code)]
pub fn global_release(slot: usize) {
    SLOT_TRACKER.lock().dealloc(slot)
}

use crate::{arch::SLOTS, simple_mutex::Mutex};
use bitset_core::BitSet;

struct SlotTracker {
    bitmap: [u32; SLOTS / 32],
}

static SLOT_TRACKER: Mutex<SlotTracker> = Mutex::new(SlotTracker {
    bitmap: [0; SLOTS / 32],
});

use crate::slot::ALLOC_START;

impl SlotTracker {
    fn alloc(&mut self) -> Option<usize> {
        for slot in ALLOC_START..self.bitmap.bit_len() {
            if !self.bitmap.bit_test(slot) {
                self.bitmap.bit_set(slot);
                return Some(slot);
            }
        }
        None
    }

    fn dealloc(&mut self, slot: usize) {
        self.bitmap.bit_reset(slot);
    }
}
