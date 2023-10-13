use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

use twizzler_abi::arch::SLOTS;

pub fn early_slot_alloc() -> Option<usize> {
    EARLY_SLOT_ALLOC.with_inner_mut(|inner| inner.get_new())
}

pub fn mark_slot_reserved(slot: usize) {
    EARLY_SLOT_ALLOC.with_inner_mut(|inner| inner.set(slot))
}

struct EarlySlotAllocatorInner {
    slots: [u8; SLOTS / 8],
}

impl EarlySlotAllocatorInner {
    const fn new() -> Self {
        Self {
            slots: [0u8; SLOTS / 8],
        }
    }

    fn test(&self, slot: usize) -> bool {
        self.slots[slot / 8] & (1 << (slot % 8)) != 0
    }

    fn set(&mut self, slot: usize) {
        self.slots[slot / 8] |= 1 << (slot % 8)
    }

    fn _release(&mut self, slot: usize) {
        self.slots[slot / 8] &= !(1 << (slot % 8));
    }

    fn get_new(&mut self) -> Option<usize> {
        for s in 0..SLOTS {
            if !self.test(s) {
                self.set(s);
                return Some(s);
            }
        }
        None
    }
}

struct EarlySlotAllocator {
    lock: AtomicBool,
    inner: UnsafeCell<EarlySlotAllocatorInner>,
}

impl EarlySlotAllocator {
    fn with_inner_mut<R>(&self, f: impl FnOnce(&mut EarlySlotAllocatorInner) -> R) -> R {
        while self.lock.swap(true, Ordering::SeqCst) {
            core::hint::spin_loop()
        }
        let r = f(unsafe { self.inner.get().as_mut().unwrap() });
        self.lock.store(false, Ordering::SeqCst);
        r
    }
}

static EARLY_SLOT_ALLOC: EarlySlotAllocator = EarlySlotAllocator {
    lock: AtomicBool::new(false),
    inner: UnsafeCell::new(EarlySlotAllocatorInner::new()),
};

unsafe impl Sync for EarlySlotAllocator {}
