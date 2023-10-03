//! Implements a global slot allocator, simple enough for this runtime.

/// Allocate a slot in the address space where we could map a new object.
pub fn global_allocate() -> Option<usize> {
    SLOT_TRACKER.lock().alloc()
}

/// Release a slot for reuse.
#[allow(dead_code)]
pub fn global_release(slot: usize) {
    SLOT_TRACKER.lock().dealloc(slot)
}

use crate::{arch::SLOTS, runtime::simple_mutex::Mutex, object::{MAX_SIZE, NULLPAGE_SIZE}, aux::KernelInitInfo};
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

/// Return the vaddr range of a slot (start address, end address).
pub(crate) fn slot_to_start_and_meta(slot: usize) -> (usize, usize) {
    let start = slot * MAX_SIZE;
    let end = (slot + 1) * MAX_SIZE - NULLPAGE_SIZE;
    (start, end)
}

/// Get the initial kernel info for init. Only works for init.
pub fn get_kernel_init_info() -> &'static KernelInitInfo {
    let (start, _) = slot_to_start_and_meta(crate::slot::RESERVED_KERNEL_INIT);
    unsafe { ((start + NULLPAGE_SIZE) as *const KernelInitInfo).as_ref().unwrap() }
}
