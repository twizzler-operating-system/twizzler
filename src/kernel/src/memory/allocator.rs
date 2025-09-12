use alloc::alloc::{GlobalAlloc, Layout};
use core::{
    mem::size_of,
    panic,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Duration,
};

use slabmalloc::{AllocationError, Allocator, LargeObjectPage, ObjectPage, ZoneAllocator};
use twizzler_abi::trace::{KernelAllocationEvent, TraceEntryFlags, TraceKind};

use super::context::{Context, KernelMemoryContext};
use crate::{
    instant::Instant,
    spinlock::Spinlock,
    thread::current_thread_ref,
    trace::{
        mgr::{TraceEvent, TRACE_MGR},
        new_trace_entry,
    },
};

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

pub struct KernelAllocator<Ctx: KernelMemoryContext + 'static> {
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

#[thread_local]
static SKIP: AtomicBool = AtomicBool::new(false);

fn trace_kalloc(layout: Layout, time: Duration, is_free: bool) {
    let Some(ct) = current_thread_ref() else {
        return;
    };
    let _guard = ct.enter_critical();
    if SKIP.swap(true, Ordering::SeqCst) {
        return;
    }
    if layout.size() == 56 && false && current_thread_ref().is_some() {
        crate::panic::backtrace(false, None);
    }
    if TRACE_MGR.any_enabled(TraceKind::Kernel, twizzler_abi::trace::KERNEL_ALLOC) {
        let data = KernelAllocationEvent {
            layout,
            duration: time.into(),
            is_free,
        };
        let entry = new_trace_entry(
            TraceKind::Kernel,
            twizzler_abi::trace::KERNEL_ALLOC,
            TraceEntryFlags::HAS_DATA,
        );
        TRACE_MGR.async_enqueue(TraceEvent::new_with_data(entry, data));
    }
    SKIP.store(false, Ordering::SeqCst);
}

unsafe impl<Ctx: KernelMemoryContext + 'static> GlobalAlloc for KernelAllocator<Ctx> {
    #[track_caller]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let start = Instant::now();
        let ret = {
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
        };
        let end = Instant::now();
        if false && current_thread_ref().is_some_and(|ct| ct.id() > 10) {
            emerglogln!(
                "{}: alloc: {}ns",
                current_thread_ref().unwrap().id(),
                (end - start).as_nanos()
            );
        }
        trace_kalloc(layout, end - start, false);
        ret
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let start = Instant::zero();
        {
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
            };
        }
        trace_kalloc(layout, Instant::zero() - start, false);
    }
}

#[global_allocator]
pub static SLAB_ALLOCATOR: KernelAllocator<Context> = KernelAllocator {
    inner: Spinlock::new(None),
};

pub fn init(ctx: &'static Context) {
    *SLAB_ALLOCATOR.inner.lock() = Some(KernelAllocatorInner {
        ctx,
        zone: ZoneAllocator::new(),
    });
}
