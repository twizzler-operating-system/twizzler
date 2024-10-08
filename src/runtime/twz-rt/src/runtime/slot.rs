//! Slot allocator. This proceeds in two phases. During the initialization phase, before the runtime
//! is marked ready, we use early slot allocation. After the runtime is ready, we use normal slot
//! allocation. Right before switching, the runtime must call in and initialize the proper slot
//! allocator.
//!
//! Slots are organized into pairs, (0,1), (2,3), (4,5), ..., (n-2,n-1). This is because the dynamic
//! linker needs to be able to load an ELF into adjacent objects in virtual memory, and is not
//! fundamental to Twizzler. To allocate single slots, we allocate a pair and split it, recording
//! one of the slots as available for single allocation, and returning the other. When a single slot
//! is released, it also gets marked as available for single allocation. However, eventually we'll
//! need to consolidate the single slots back into pairs, or we will run out. When the number of
//! single slots allocated from pairs grows past a high watermark, we do a GC run on the slot list,
//! which sorts the list and then finds and removes pairs, freeing those pairs back up for future
//! allocation.
//!
//! One thing that makes this tricky is that we cannot allocate memory within the slot allocator, as
//! we hold a lock on it, and the allocator might call us if it needs another object for allocating
//! memory. Thus we must be careful during operation to not allocate memory. We manage this by being
//! a bit wasteful: the slot allocator reserves two vectors ahead of time, each of capacity SLOTS
//! (which is the max number of slots we can have). The first is a stack of single allocated slots,
//! and the second is used during the GC pass described above.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

use tracing::trace;
use twizzler_abi::arch::SLOTS;

use super::{ReferenceRuntime, RuntimeState};
use crate::{preinit::preinit_abort, preinit_println};

fn early_slot_alloc() -> Option<usize> {
    Some(EARLY_SLOT_ALLOC.next.fetch_add(1, Ordering::SeqCst))
}

/// Mark a slot as reserved. This probably should only be called by the monitor initialization code.
pub fn mark_slot_reserved(slot: usize) {
    // Do a simple reservation. The bootstrap is likely to reserve slots in-order,
    // so we can get away just starting our slots above the bootstrap slots.
    let current = EARLY_SLOT_ALLOC.next.load(Ordering::SeqCst);
    if slot >= current {
        EARLY_SLOT_ALLOC.next.store(slot + 1, Ordering::SeqCst);
    }
}

// Simple incremental allocator.
struct EarlySlotAllocator {
    next: AtomicUsize,
}

impl EarlySlotAllocator {}

static EARLY_SLOT_ALLOC: EarlySlotAllocator = EarlySlotAllocator {
    next: AtomicUsize::new(0),
};

struct SlotAllocatorInner {
    pairs: [u8; (SLOTS / 2) / 8],
    singles: Vec<usize>,
    singles_aux: Vec<usize>,
}

impl SlotAllocatorInner {
    const fn new() -> Self {
        Self {
            pairs: [0; SLOTS / 2 / 8],
            singles: Vec::new(),
            singles_aux: Vec::new(),
        }
    }

    fn test(&self, pair: usize) -> bool {
        self.pairs[pair / 8] & (1 << (pair % 8)) != 0
    }

    fn set(&mut self, pair: usize) {
        self.pairs[pair / 8] |= 1 << (pair % 8)
    }

    fn release_pair(&mut self, first_slot: usize) {
        assert!(first_slot % 2 == 0);
        let pair = first_slot / 2;
        self.pairs[pair / 8] &= !(1 << (pair % 8));
    }

    fn alloc_pair(&mut self) -> Option<(usize, usize)> {
        for p in 0..(SLOTS / 2) {
            if !self.test(p) {
                self.set(p);
                return Some((p * 2, p * 2 + 1));
            }
        }
        None
    }

    fn alloc_single(&mut self) -> Option<usize> {
        if let Some(idx) = self.singles.pop() {
            return Some(idx);
        }

        let pair = self.alloc_pair()?;
        trace!("slot allocator: splitting pair ({}, {})", pair.0, pair.1);
        self.singles.push(pair.0);
        Some(pair.1)
    }

    fn release_single(&mut self, slot: usize) {
        self.singles.push(slot);
        self.maybe_gc_singles();
    }

    // TODO: tune this!
    const HIGH_WATERMARK: usize = SLOTS / 4;
    const SINGLES_CAPACITY: usize = SLOTS;

    fn maybe_gc_singles(&mut self) {
        if self.singles.len() < Self::HIGH_WATERMARK {
            return;
        }
        trace!(
            "slot allocator: GC single slots (len = {})",
            self.singles.len()
        );
        // Step 1: setup the aux vector and sort the singles. Use unstable sort because it doesn't
        // allocate memory.
        self.singles_aux.truncate(0);
        self.singles.sort_unstable();

        // Step 2: collect a list of valid pairs by iterating over all windows of size 2 and
        // checking if a window contains a pair. Note that this is exactly correct and not
        // an overcount because we know that each slot in here is unique, so imagine:
        // [2,3,4,5]. This will produce pairs (2,3) and (4,5), even though it considers and sees
        // (3,4) as a pair of consecutive indices. But (3,4) is not a valid pair because it
        // does not start with an even number, and all valid pairs do. [1,2,3,4] => (2,3),
        // [2,4,5] => (4,5)
        let pair_firsts = self.singles.array_windows::<2>().filter_map(|maybe_pair| {
            if (maybe_pair[0] % 2 == 0) && maybe_pair[1] == maybe_pair[0] + 1 {
                Some(maybe_pair[0])
            } else {
                None
            }
        });
        // Use the preallocated aux vector to collect the pair list.
        self.singles_aux.extend(pair_firsts);

        // Step 3: remove all pairs from the single list, and free them.
        for pf in &self.singles_aux {
            let index = self.singles.binary_search(pf).unwrap();
            let old_a = self.singles.remove(index);
            let old_b = self.singles.remove(index + 1);
            assert_eq!(old_a + 1, old_b);
            assert_eq!(old_a, *pf);
            assert_eq!(old_a % 2, 0);
            let pair = *pf / 2;
            self.pairs[pair / 8] &= !(1 << (pair % 8));
        }

        trace!(
            "slot allocator: GC single slots recovered {} pairs (single slots len = {})",
            self.singles_aux.len(),
            self.singles.len()
        );
    }
}

struct SlotAllocator {
    inner: Mutex<SlotAllocatorInner>,
}

static SLOT_ALLOCATOR: SlotAllocator = SlotAllocator {
    inner: Mutex::new(SlotAllocatorInner::new()),
};

#[allow(dead_code)]
impl ReferenceRuntime {
    pub(crate) fn init_slots(&self) {
        // pre-allocate the slot vectors
        let singles = Vec::with_capacity(SlotAllocatorInner::SINGLES_CAPACITY);
        let singles_aux = Vec::with_capacity(SlotAllocatorInner::SINGLES_CAPACITY);
        let mut inner = SLOT_ALLOCATOR.inner.lock().unwrap();
        inner.singles = singles;
        inner.singles_aux = singles_aux;
        for i in 0..(EARLY_SLOT_ALLOC.next.load(Ordering::SeqCst) / 2 + 1) {
            inner.set(i);
        }
    }

    /// Allocate a slot, returning it's number if one is available.
    pub fn allocate_slot(&self) -> Option<usize> {
        if self.state().contains(RuntimeState::READY) {
            SLOT_ALLOCATOR.inner.lock().unwrap().alloc_single()
        } else {
            early_slot_alloc()
        }
    }

    /// Release a slot.
    pub fn release_slot(&self, slot: usize) {
        if self.state().contains(RuntimeState::READY) {
            SLOT_ALLOCATOR.inner.lock().unwrap().release_single(slot)
        }
        // early alloc has no ability to release slots
    }

    /// Allocate a pair of adjacent slots, returning their numbers if a pair is available.
    /// The returned tuple will always be of form (x, x+1).
    pub fn allocate_pair(&self) -> Option<(usize, usize)> {
        if self.state().contains(RuntimeState::READY) {
            SLOT_ALLOCATOR.inner.lock().unwrap().alloc_pair()
        } else {
            preinit_println!("cannot allocate slot pairs during runtime init");
            preinit_abort();
        }
    }

    /// Release a pair. Must be of form (x, x+1).
    pub fn release_pair(&self, pair: (usize, usize)) {
        if self.state().contains(RuntimeState::READY) {
            assert_eq!(pair.0 + 1, pair.1);
            SLOT_ALLOCATOR.inner.lock().unwrap().release_pair(pair.0)
        }
        // early alloc has no ability to release slots
    }
}
