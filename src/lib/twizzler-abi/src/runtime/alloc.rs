use core::alloc::GlobalAlloc;

#[derive(Default)]
pub struct MinimalAllocator {}

impl MinimalAllocator {
    pub(super) const fn new() -> Self {
        Self {}
    }
}

unsafe impl GlobalAlloc for MinimalAllocator {
    unsafe fn alloc(&self, _layout: core::alloc::Layout) -> *mut u8 {
        todo!()
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {
        todo!()
    }
}
