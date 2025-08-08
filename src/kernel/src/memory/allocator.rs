use alloc::alloc::{GlobalAlloc, Layout};
use core::{
    mem::size_of,
    panic,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use slabmalloc::{AllocationError, Allocator, LargeObjectPage, ObjectPage, ZoneAllocator};

use super::context::{Context, KernelMemoryContext};
use crate::spinlock::Spinlock;

#[alloc_error_handler]
fn alloc_error_handler(layout: Layout) -> ! {
    panic!("allocation error: {:?}", layout)
}

const EARLY_ALLOCATION_SIZE: usize = 1024 * 1024 * 2;
#[repr(align(64))]
#[derive(Copy, Clone)]
struct AlignedU8(u8);

static mut EARLY_ALLOCATION_AREA: [AlignedU8; EARLY_ALLOCATION_SIZE] =
    [AlignedU8(0); EARLY_ALLOCATION_SIZE];
static EARLY_ALLOCATION_PTR: AtomicUsize = AtomicUsize::new(0);

struct KernelAllocatorInner<Ctx: KernelMemoryContext + 'static> {
    ctx: &'static Ctx,
    zone: ZoneAllocator<'static>,
}

struct KernelAllocator<Ctx: KernelMemoryContext + 'static> {
    inner: Spinlock<Option<KernelAllocatorInner<Ctx>>>,
}

impl<Ctx: KernelMemoryContext + 'static> KernelAllocator<Ctx> {
    fn early_alloc(&self, layout: Layout) -> *mut u8 {
        let start = EARLY_ALLOCATION_PTR.load(Ordering::SeqCst);
        let start = crate::utils::align(start, layout.align());
        EARLY_ALLOCATION_PTR.store(start + layout.size(), Ordering::SeqCst);
        if start + layout.size() >= EARLY_ALLOCATION_SIZE {
            panic!("out of early memory");
        }
        // Safety: this is safe because we are only ever handing out unique slices of this region,
        // and this is then used as allocated memory. Also, at this point, there is only 1 thread.
        #[allow(static_mut_refs)]
        unsafe {
            EARLY_ALLOCATION_AREA.as_mut_ptr().add(start) as *mut u8
        }
    }
}

impl<Ctx: KernelMemoryContext + 'static> KernelAllocatorInner<Ctx> {
    fn allocate_page(&mut self) -> &'static mut ObjectPage<'static> {
        let size = size_of::<ObjectPage>();
        let chunk = self
            .ctx
            .allocate_chunk(Layout::from_size_align(size, size).unwrap())
            .as_ptr();
        unsafe { &mut *(chunk as *mut ObjectPage<'static>) }
    }

    fn allocate_large_page(&mut self) -> &'static mut LargeObjectPage<'static> {
        let size = size_of::<LargeObjectPage>();
        let chunk = self
            .ctx
            .allocate_chunk(Layout::from_size_align(size, size).unwrap())
            .as_ptr();
        unsafe { &mut *(chunk as *mut LargeObjectPage<'static>) }
    }
}

unsafe impl<Ctx: KernelMemoryContext + 'static> GlobalAlloc for KernelAllocator<Ctx> {
    #[track_caller]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut inner = self.inner.lock();

        if inner.is_none() {
            return self.early_alloc(layout);
        }
        let inner = inner.as_mut().unwrap();
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
            _ => inner.ctx.allocate_chunk(layout).as_ptr(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut inner = self.inner.lock();
        if inner.is_none() {
            /* freeing memory in early init. Sadly, we just have to leak it. */
            return;
        }
        let inner = inner.as_mut().unwrap();
        if ptr.is_null() {
            return;
        }
        let nn = NonNull::new(ptr).unwrap();
        match layout.size() {
            0..=ZoneAllocator::MAX_ALLOC_SIZE => {
                inner
                    .zone
                    .deallocate(nn, layout)
                    .expect("failed to deallocate memory");
            }
            _ => inner
                .ctx
                .deallocate_chunk(layout, NonNull::new(ptr).unwrap()),
        }
    }
}

#[global_allocator]
static SLAB_ALLOCATOR: KernelAllocator<Context> = KernelAllocator {
    inner: Spinlock::new(None),
};

pub fn init(ctx: &'static Context) {
    *SLAB_ALLOCATOR.inner.lock() = Some(KernelAllocatorInner {
        ctx,
        zone: ZoneAllocator::new(),
    });
}
