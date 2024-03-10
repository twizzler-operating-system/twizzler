//! This module handles allocating memory within a compartment for use by libraries within
//! that compartment.

use std::{alloc::Layout, ptr::NonNull};

use talc::Span;

use crate::library::BackingData;

use super::Compartment;

impl<Backing: BackingData> Compartment<Backing> {
    fn add_alloc_object(&mut self) {
        let new_data = Backing::new_data();

        if let Ok(new_data) = new_data {
            unsafe {
                let memory = Span::from_base_size(new_data.data().0, new_data.data().1);
                // We ensure that we do not meet the conditions for this to return Err.
                let _ = self.allocator.claim(memory);
            }
            self.alloc_objects.push(new_data);
        }
    }

    pub(crate) unsafe fn alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        if let Ok(alloc) = self.allocator.malloc(layout) {
            Some(alloc)
        } else {
            self.add_alloc_object();
            self.allocator.malloc(layout).ok()
        }
    }

    pub(crate) unsafe fn _dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        self.allocator.free(ptr, layout)
    }
}
