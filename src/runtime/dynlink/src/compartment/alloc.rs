//! This module handles allocating memory within a compartment for use by libraries within
//! that compartment.

use std::{
    alloc::{AllocError, Allocator, Layout},
    ptr::NonNull,
};

use talc::Span;
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_object::{Object, ObjectInitFlags, Protections};

use super::{Compartment, CompartmentInner, CompartmentRef};

fn new_object() -> Option<Object<u8>> {
    let id = sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        ),
        &[],
        &[],
    )
    .ok()?;

    Object::init_id(
        id,
        Protections::READ | Protections::WRITE,
        ObjectInitFlags::empty(),
    )
    .ok()
}

impl CompartmentInner {
    fn add_alloc_object(&mut self) {
        if let Some(obj) = new_object() {
            unsafe {
                let memory = Span::new(
                    obj.base_mut_unchecked(),
                    (obj.base_mut_unchecked() as *mut u8).add(MAX_SIZE - NULLPAGE_SIZE),
                );
                // We ensure that we do not meet the conditions for this to return Err.
                let _ = self.allocator.claim(memory);
            }
            self.alloc_objects.insert(0, obj);
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

    pub(crate) unsafe fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) {
        self.allocator.free(ptr, layout)
    }
}

#[allow(dead_code)]
impl Compartment {
    pub(crate) fn make_box<T>(self: &CompartmentRef, data: T) -> Option<Box<T, CompartmentAlloc>> {
        Some(Box::new_in(data, CompartmentAlloc { comp: self.clone() }))
    }

    pub(crate) fn make_box_slice<T: Clone>(
        self: &CompartmentRef,
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
