use std::{alloc::GlobalAlloc, ptr::NonNull, sync::atomic::Ordering};

use twizzler_abi::object::ObjID;

use super::{ReferenceRuntime, RuntimeState};

mod ferroc;
mod talc;

pub use talc::LOCAL_ALLOCATOR;

unsafe impl GlobalAlloc for ReferenceRuntime {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if !self.state().contains(RuntimeState::READY)
            || self.state().contains(RuntimeState::IS_MONITOR)
        {
            return LOCAL_ALLOCATOR.alloc(layout);
        }

        //let start_time = Instant::now();
        let r = ferroc::TwzFerroc
            .allocate(layout)
            .map(|nn| nn.as_ptr())
            .unwrap_or(core::ptr::null_mut())
            .cast::<u8>();
        //let end_time = Instant::now();
        //trace_runtime_alloc(r.addr(), layout, end_time - start_time, false);
        r
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        if !self.state().contains(RuntimeState::READY)
            || self.state().contains(RuntimeState::IS_MONITOR)
        {
            return LOCAL_ALLOCATOR.alloc_zeroed(layout);
        }

        //let start_time = Instant::now();
        let r = ferroc::TwzFerroc
            .allocate_zeroed(layout)
            .map(|nn| nn.as_ptr())
            .unwrap_or(core::ptr::null_mut())
            .cast::<u8>();
        //let end_time = Instant::now();
        //trace_runtime_alloc(r.addr(), layout, end_time - start_time, false);
        r
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        if !self.state().contains(RuntimeState::READY) {
            return;
        }

        if self.state().contains(RuntimeState::IS_MONITOR) {
            return LOCAL_ALLOCATOR.dealloc(ptr, layout);
        }

        if let Some(ptr) = NonNull::new(ptr) {
            //let start_time = Instant::now();
            ferroc::TwzFerroc.deallocate(ptr, layout);
            //let end_time = Instant::now();
            //trace_runtime_alloc(ptr.addr().into(), layout, end_time - start_time, true);
        }
    }
}

impl ReferenceRuntime {
    pub(crate) fn register_bootstrap_alloc(&self, slot: usize) {
        LOCAL_ALLOCATOR
            .bootstrap_alloc_slot
            .store(slot, Ordering::SeqCst);
    }

    pub fn get_id_from_heap_ptr(&self, ptr: *const u8) -> Option<ObjID> {
        LOCAL_ALLOCATOR.get_id_from_ptr(ptr)
    }
}
