//! Implements the allocation part of the core runtime trait. We use talc as our allocator, creating new objects for it to
//! claim when it runs out of memory.

use core::{alloc::GlobalAlloc, ptr::NonNull};

use talc::{OomHandler, Span};

use crate::{
    object::{InternalObject, MAX_SIZE, NULLPAGE_SIZE},
    runtime::simple_mutex::Mutex,
};

pub struct MinimalAllocator {
    imp: Mutex<talc::Talc<MinimalOomHandler>>,
}

pub struct MinimalOomHandler {}

// Null page + Meta page + 2 extra pages (reserving 1 for FOT and one for base data).
const ALLOC_OBJ_REG_SIZE: usize = MAX_SIZE - NULLPAGE_SIZE * 4;

impl OomHandler for MinimalOomHandler {
    fn handle_oom(talc: &mut talc::Talc<Self>, layout: core::alloc::Layout) -> Result<(), ()> {
        if layout.size() > ALLOC_OBJ_REG_SIZE {
            return Err(());
        }
        let obj = InternalObject::<u8>::create_data_and_map().ok_or(())?;
        // Save room for base data.
        let start = unsafe { (obj.base_mut() as *mut u8).add(NULLPAGE_SIZE) };
        let span = Span::new(start, unsafe { start.add(ALLOC_OBJ_REG_SIZE) });
        unsafe {
            talc.claim(span)?;
            // Drop this because its now in the allocator, unrecoverable
            core::mem::forget(obj);
        }

        Ok(())
    }
}

impl Default for MinimalAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl MinimalAllocator {
    pub(super) const fn new() -> Self {
        Self {
            imp: Mutex::new(talc::Talc::new(MinimalOomHandler {})),
        }
    }
}

unsafe impl GlobalAlloc for MinimalAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        self.imp
            .lock()
            .malloc(layout)
            .expect("memory allocation failed")
            .as_ptr()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        if let Some(ptr) = NonNull::new(ptr) {
            self.imp.lock().free(ptr, layout)
        }
    }
}
