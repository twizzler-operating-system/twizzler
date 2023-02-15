use alloc::alloc::{GlobalAlloc, Layout};
use core::{
    intrinsics::transmute,
    panic,
    ptr::{self, NonNull},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};
use slabmalloc::{AllocationError, Allocator, LargeObjectPage, ObjectPage, ZoneAllocator};

use crate::spinlock::Spinlock;

use super::context::KernelMemoryContext;

#[alloc_error_handler]
fn alloc_error_handler(layout: Layout) -> ! {
    panic!("allocation error: {:?}", layout)
}

const EARLY_ALLOCATION_SIZE: usize = 1024 * 1024 * 2;
static mut EARLY_ALLOCATION_AREA: [u8; EARLY_ALLOCATION_SIZE] = [0; EARLY_ALLOCATION_SIZE];
static EARLY_ALLOCATION_PTR: AtomicUsize = AtomicUsize::new(0);

struct KernelAllocatorInner<Ctx: KernelMemoryContext> {
    ctx: Ctx,
    zone: ZoneAllocator<'static>,
}

struct KernelAllocator<Ctx: KernelMemoryContext> {
    inner: Spinlock<Option<KernelAllocatorInner<Ctx>>>,
}

impl<Ctx: KernelMemoryContext> KernelAllocator<Ctx> {
    fn early_alloc(&self, layout: Layout) -> *mut u8 {
        let start = EARLY_ALLOCATION_PTR.load(Ordering::SeqCst);
        let start = crate::utils::align(start, layout.align());
        EARLY_ALLOCATION_PTR.store(start + layout.size(), Ordering::SeqCst);
        if start + layout.size() >= EARLY_ALLOCATION_SIZE {
            panic!("out of early memory");
        }
        unsafe { EARLY_ALLOCATION_AREA.as_mut_ptr().add(start) }
    }
}

impl<Ctx: KernelMemoryContext> KernelAllocatorInner<Ctx> {
    fn allocate_page(&mut self) -> &'static mut ObjectPage<'static> {
        let chunk = self.ctx.allocate_chunk(
            Layout::from_size_align(
                ZoneAllocator::MAX_BASE_ALLOC_SIZE,
                ZoneAllocator::MAX_BASE_ALLOC_SIZE,
            )
            .unwrap(),
        );
        unsafe { &mut *(chunk as *mut ObjectPage<'static>) }
    }

    fn allocate_large_page(&mut self) -> &'static mut LargeObjectPage<'static> {
        let chunk = self.ctx.allocate_chunk(
            Layout::from_size_align(
                ZoneAllocator::MAX_BASE_ALLOC_SIZE,
                ZoneAllocator::MAX_BASE_ALLOC_SIZE,
            )
            .unwrap(),
        );
        unsafe { &mut *(chunk as *mut LargeObjectPage<'static>) }
    }
}

unsafe impl<Ctx: KernelMemoryContext> GlobalAlloc for KernelAllocator<Ctx> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let inner = self.inner.lock();
        if inner.is_none() {
            return self.early_alloc(layout);
        }
        let inner = inner.as_ref().unwrap();
        match layout.size() {
            0..=ZoneAllocator::MAX_ALLOC_SIZE => match inner.zone.allocate(layout) {
                Ok(nptr) => nptr.as_ptr(),
                Err(AllocationError::OutOfMemory) => {
                    if layout.size() <= ZoneAllocator::MAX_BASE_ALLOC_SIZE {
                        let new_page = inner.allocate_page();
                        inner
                            .zone
                            .refill(layout, new_page)
                            .expect("failed to refill zone allocator");
                        inner
                            .zone
                            .allocate(layout)
                            .expect("allocation failed after refill")
                            .as_ptr()
                    } else {
                        let new_page = inner.allocate_large_page();
                        inner
                            .zone
                            .refill_large(layout, new_page)
                            .expect("failed to refill zone allocator");
                        inner
                            .zone
                            .allocate(layout)
                            .expect("allocation failed after refill")
                            .as_ptr()
                    }
                }
                Err(AllocationError::InvalidLayout) => {
                    panic!("cannot allocate this layout {:?}", layout)
                }
            },
            _ => inner.ctx.allocate_chunk(layout),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let inner = self.inner.lock();
        if inner.is_none() {
            /* freeing memory in early init. Sadly, we just have to leak it. */
            return;
        }
        let inner = inner.as_ref().unwrap();
        let nonnull = ptr.as_ref();
        if nonnull.is_none() {
            return;
        }
        match layout.size() {
            0..=ZoneAllocator::MAX_ALLOC_SIZE => {
                if let Some(nptr) = NonNull::new(ptr) {
                    inner
                        .zone
                        .deallocate(nptr, layout)
                        .expect("failed to deallocate memory");
                }
            }
            _ => inner.ctx.deallocate_chunk(layout, ptr),
        }
    }
}
