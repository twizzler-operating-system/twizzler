//! This module handles allocating memory within a compartment for use by libraries within
//! that compartment.

use std::{
    alloc::{AllocError, Allocator, Layout},
    ptr::NonNull,
};

use talc::Span;

use crate::library::BackingData;

use super::Compartment;

impl<Backing: BackingData> Compartment<Backing> {
    fn add_alloc_object(&mut self) {
        let new_data = Backing::new_data();

        unsafe {
            let memory = Span::from_base_size(new_data.0, new_data.1);
            // We ensure that we do not meet the conditions for this to return Err.
            let _ = self.allocator.claim(memory);
        }
        self.alloc_objects.push(new_data);
    }

    pub(crate) unsafe fn alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        if let Ok(alloc) = self.allocator.malloc(layout) {
            Some(alloc)
        } else {
            self.add_alloc_object();
            self.allocator.malloc(layout).ok()
        }
    }

    pub(crate) unsafe fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        self.allocator.free(ptr, layout)
    }
}

/*
#[allow(dead_code)]
impl<Backing: BackingData> Compartment<Backing> {
    pub(crate) fn make_box<T>(&self, data: T) -> Option<Box<T, CompartmentAlloc>> {
        Some(Box::new_in(data, CompartmentAlloc { comp: self.clone() }))
    }

    pub(crate) fn make_box_slice<T: Clone>(
        &self,
        data: &[T],
    ) -> Option<Box<[T], CompartmentAlloc>> {
        let mut vec = Vec::<T, CompartmentAlloc>::new_in(CompartmentAlloc { comp: self.clone() });
        vec.extend_from_slice(data);
        Some(vec.into_boxed_slice())
    }
}

pub(crate) struct CompartmentAlloc {
    comp: CompartmentRef,
}

impl From<&CompartmentRef> for CompartmentAlloc {
    fn from(value: &CompartmentRef) -> Self {
        Self {
            comp: value.clone(),
        }
    }
}

unsafe impl Allocator for CompartmentAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.comp
            .with_inner_mut(|inner| {
                match layout.size() {
                    0 => Ok(NonNull::slice_from_raw_parts(layout.dangling(), 0)),
                    // SAFETY: `layout` is non-zero in size,
                    size => unsafe {
                        inner
                            .alloc(layout)
                            .map(|p| NonNull::slice_from_raw_parts(p, size))
                            .ok_or(AllocError)
                    },
                }
            })
            .map_err(|_| AllocError)
            .flatten()
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let _ = self.comp.with_inner_mut(|inner| inner.dealloc(ptr, layout));
    }
}
*/
