use alloc::alloc::{GlobalAlloc, Layout};
use core::{
    panic,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering},
    time::Duration,
};

use twizzler_abi::trace::{KernelAllocationEvent, TraceEntryFlags, TraceKind};
use twizzler_rt_abi::error::TwzError;

use super::context::KernelMemoryContext;
use crate::{
    instant::Instant,
    interrupt::with_disabled,
    memory::context::{ContextRef, kernel_context},
    once::Once,
    processor::tls_ready,
    thread::current_thread_ref,
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

#[alloc_error_handler]
fn alloc_error_handler(layout: Layout) -> ! {
    panic!("allocation error: {:?}", layout)
}

const EARLY_ALLOCATION_SIZE: usize = 1024 * 1024 * 1;
#[repr(align(64))]
#[derive(Copy, Clone)]
struct AlignedU8(u8);

#[repr(align(64))]
struct EarlyAllocator {
    early_allocation_region: [AlignedU8; EARLY_ALLOCATION_SIZE],
    early_allocation_ptr: AtomicUsize,
}

static EARLY_ALLOCATOR: EarlyAllocator = EarlyAllocator {
    early_allocation_region: [AlignedU8(0); EARLY_ALLOCATION_SIZE],
    early_allocation_ptr: AtomicUsize::new(0),
};

impl EarlyAllocator {
    fn early_alloc(&self, layout: Layout) -> *mut u8 {
        if let Some(ctx) = KERNEL_CTX.poll() {
            return ctx.allocate_chunk(layout).unwrap().as_ptr();
        }
        let start = loop {
            let current = self.early_allocation_ptr.load(Ordering::SeqCst);
            let start = crate::utils::align(current, layout.align());
            if self
                .early_allocation_ptr
                .compare_exchange(
                    current,
                    start + layout.size(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                break start;
            }
        };
        if start + layout.size() >= EARLY_ALLOCATION_SIZE {
            panic!("out of early memory");
        }
        // Safety: this is safe because we are only ever handing out unique slices of this region,
        // and this is then used as allocated memory. Also, at this point, there is only 1 thread.
        #[allow(static_mut_refs)]
        unsafe {
            self.early_allocation_region.as_ptr().add(start) as *mut u8
        }
    }
}

pub struct KernelAllocator {
    early_allocated_bytes: AtomicUsize,
}

static KERNEL_CTX: Once<&'static ContextRef> = Once::new();

impl KernelAllocator {
    const fn new() -> Self {
        Self {
            early_allocated_bytes: AtomicUsize::new(0),
        }
    }

    fn ctx(&self) -> &'static ContextRef {
        KERNEL_CTX.call_once(|| kernel_context())
    }
}

unsafe impl ferroc::base::BaseAlloc for KernelAllocator {
    const IS_ZEROED: bool = false;

    type Handle = NonNull<u8>;

    type Error = TwzError;

    fn allocate(
        &self,
        layout: Layout,
        _commit: bool,
    ) -> Result<ferroc::base::Chunk<Self>, Self::Error> {
        let ptr = self.ctx().allocate_chunk(layout)?;
        logln!(
            "kernel allocator: allocated {} bytes at {:p}",
            layout.size(),
            ptr.as_ptr()
        );
        Ok(unsafe { ferroc::base::Chunk::new(ptr, layout, ptr) })
    }

    unsafe fn deallocate(chunk: &mut ferroc::base::Chunk<Self>) {
        unsafe { kernel_context().deallocate_chunk(chunk.layout(), chunk.pointer().cast()) };
    }
}

#[thread_local]
static SKIP: AtomicBool = AtomicBool::new(false);

/// Live late-heap bytes, spread over cpu-indexed lines so an allocation does not bounce one shared
/// counter between cpus. A slot can go negative (freed on another cpu than allocated); the sum is
/// exact.
const KALLOC_SLOTS: usize = 64;

#[repr(align(64))]
struct KallocSlot(AtomicIsize);

static KALLOC_BYTES: [KallocSlot; KALLOC_SLOTS] =
    [const { KallocSlot(AtomicIsize::new(0)) }; KALLOC_SLOTS];

fn kalloc_bytes_add(delta: isize) {
    let slot = crate::processor::mp::current_processor().id as usize % KALLOC_SLOTS;
    KALLOC_BYTES[slot].0.fetch_add(delta, Ordering::Relaxed);
}

fn kalloc_traced() -> bool {
    TRACE_MGR.any_enabled(TraceKind::Kernel, twizzler_abi::trace::KERNEL_ALLOC)
}

fn trace_kalloc(layout: Layout, time: Duration, is_free: bool) {
    let Some(ct) = current_thread_ref() else {
        return;
    };
    let _guard = ct.enter_critical();
    if SKIP.swap(true, Ordering::SeqCst) {
        return;
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

ferroc::config!(pub FerrocAllocator => KernelAllocator);

struct GlobalAllocWrapper;

unsafe impl GlobalAlloc for GlobalAllocWrapper {
    #[track_caller]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let early = !tls_ready() || KERNEL_CTX.poll().is_none();
        let inner = FerrocAllocator.base();

        if early {
            inner
                .early_allocated_bytes
                .fetch_add(layout.size(), Ordering::SeqCst);
            return EARLY_ALLOCATOR.early_alloc(layout);
        }
        let start = kalloc_traced().then(Instant::now);
        kalloc_bytes_add(layout.size() as isize);

        let ret = if layout.size() >= ferroc::config::SLAB_SIZE {
            inner.ctx().allocate_chunk(layout).unwrap().as_ptr()
        } else {
            with_disabled(|| {
                let _guard = current_thread_ref().map(|ct| ct.enter_critical());
                FerrocAllocator.allocate(layout).unwrap().as_ptr().cast()
            })
        };

        if let Some(start) = start {
            trace_kalloc(layout, Instant::now() - start, false);
        }
        ret
    }

    #[track_caller]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        {
            let early = !tls_ready() || KERNEL_CTX.poll().is_none();
            if early {
                return;
            }
            if ptr.is_null() {
                return;
            }

            let inner = FerrocAllocator.base();
            kalloc_bytes_add(-(layout.size() as isize));
            if layout.size() >= ferroc::config::SLAB_SIZE {
                unsafe {
                    inner
                        .ctx()
                        .deallocate_chunk(layout, NonNull::new(ptr).unwrap())
                };
                return;
            }
            let nn = NonNull::new(ptr).unwrap();
            with_disabled(|| {
                let _guard = current_thread_ref().map(|ct| ct.enter_critical());
                unsafe { FerrocAllocator.deallocate(nn, layout) };
            });
        }
        if kalloc_traced() {
            trace_kalloc(layout, Duration::ZERO, true);
        }
    }
}

#[global_allocator]
static GLOBAL_ALLOC: GlobalAllocWrapper = GlobalAllocWrapper;

pub fn init(ctx: &'static ContextRef) {
    KERNEL_CTX.call_once(|| ctx);
}

pub fn fill_stats(stats: &mut twizzler_abi::syscall::MemoryStats) {
    let live: isize = KALLOC_BYTES
        .iter()
        .map(|slot| slot.0.load(Ordering::Relaxed))
        .sum();
    stats.late_kalloc_bytes = live.max(0) as usize;
    stats.early_kalloc_bytes = FerrocAllocator
        .base()
        .early_allocated_bytes
        .load(Ordering::SeqCst);
}

unsafe extern "C" fn __did_init_call(id: u64) {
    logln!("did init {}", id);
}
