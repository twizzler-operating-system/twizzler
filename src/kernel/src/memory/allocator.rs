use alloc::alloc::{GlobalAlloc, Layout};
use core::{
    intrinsics::transmute,
    panic,
    ptr::{self, NonNull},
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
};
use slabmalloc::{AllocationError, Allocator, LargeObjectPage, ObjectPage, ZoneAllocator};
use x86::bits32::paging::LARGE_PAGE_SIZE;

//#[global_allocator]
//static ALLOCATOR: LockedHeap = LockedHeap::empty();

#[alloc_error_handler]
fn alloc_error_handler(layout: Layout) -> ! {
    panic!("allocation error: {:?}", layout)
}

/* TODO: arch-dep or machine-dep */
pub const HEAP_START: usize = 0xffffff0000000000;
pub const HEAP_HUGE_START: usize = 0xfffffe0000000000;

use x86_64::VirtAddr;

use super::KernelMemoryManager;

/*
pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + (HEAP_SIZE - 1);
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
    }

    Ok(())
}
*/

struct HeapPager {
    next_page: AtomicU64,
    next_large_page: AtomicU64,
    heap_start: u64,
    huge_heap_start: u64,
    huge_heap_top: AtomicU64,
    memory_manager: Option<&'static KernelMemoryManager>,
}

impl HeapPager {
    const BASE_PAGE_SIZE: usize = 4096;
    const LARGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

    fn hookup_kernel_memory_manager(&mut self, kmm: &'static KernelMemoryManager) {
        self.memory_manager = Some(kmm);
    }

    fn is_ready(&self) -> bool {
        self.memory_manager.is_some()
    }

    fn calc_page_offset(&self, num: u64, large: bool) -> u64 {
        if large {
            num * 2 * LARGE_PAGE_SIZE as u64 + self.heap_start
        } else {
            let num_sp = (LARGE_PAGE_SIZE / Self::BASE_PAGE_SIZE) as u64;
            let large_idx = num / num_sp;
            let large_off = num % num_sp;
            ((large_idx * 2) + 1) * LARGE_PAGE_SIZE as u64
                + large_off * Self::BASE_PAGE_SIZE as u64
                + self.heap_start
        }
    }

    fn map_offset(&self, offset: u64, large: bool) {
        /* TODO: can we handle mapping failure more gracefully? */
        self.memory_manager
            .expect("cannot use global memory allocator before initializing the kernel MM")
            .map_zero_pages(
                VirtAddr::new(offset),
                if large {
                    Self::LARGE_PAGE_SIZE
                } else {
                    Self::BASE_PAGE_SIZE
                },
            )
            .expect("failed to map backing memory for global heap");
    }

    fn extend_huge_heap(&self, length: usize) -> usize {
        /* TODO: can we handle mapping failure more gracefully? */
        let length = ((length - 1) & !(Self::BASE_PAGE_SIZE - 1)) + Self::BASE_PAGE_SIZE;
        let map_start = self
            .huge_heap_top
            .fetch_add(length as u64, core::sync::atomic::Ordering::SeqCst);
        self.memory_manager
            .expect("cannot use global allocator before initializing the kernel MM")
            .map_zero_pages(VirtAddr::new(map_start), length)
            .expect("failed to map backing memory for huge heap");
        length
    }

    const fn new() -> Self {
        Self {
            next_page: AtomicU64::new(0),
            next_large_page: AtomicU64::new(0),
            heap_start: HEAP_START as u64,
            memory_manager: None,
            huge_heap_start: HEAP_HUGE_START as u64,
            huge_heap_top: AtomicU64::new(HEAP_HUGE_START as u64),
        }
    }

    fn alloc_page(&mut self, large: bool) -> Option<*mut u8> {
        assert!(self.heap_start > 0);
        let next = if large {
            self.next_large_page
                .fetch_add(1, core::sync::atomic::Ordering::SeqCst)
        } else {
            self.next_page
                .fetch_add(1, core::sync::atomic::Ordering::SeqCst)
        };
        let offset = self.calc_page_offset(next, large);
        self.map_offset(offset, large);
        Some(offset as *mut u8)
    }

    fn dealloc_page(&mut self, _ptr: *mut u8, _large: bool) {
        assert!(self.heap_start > 0);
        /* TODO: actually deallocate page */
    }

    fn allocate_page(&mut self) -> Option<&'static mut ObjectPage<'static>> {
        self.alloc_page(false)
            .map(|r| unsafe { &mut *(r as *mut ObjectPage) })
    }

    #[allow(unused)]
    fn release_page(&mut self, p: &'static mut ObjectPage<'static>) {
        self.dealloc_page(p as *const ObjectPage as *mut u8, false)
    }

    fn allocate_large_page(&mut self) -> Option<&'static mut LargeObjectPage<'static>> {
        self.alloc_page(true)
            .map(|r| unsafe { &mut *(r as *mut LargeObjectPage) })
    }

    #[allow(unused)]
    fn release_large_page(&mut self, p: &'static mut LargeObjectPage<'static>) {
        self.dealloc_page(p as *const LargeObjectPage as *mut u8, true)
    }
}

static mut PAGER: HeapPager = HeapPager::new();
static mut LL_BACKUP_ALLOCATOR: spin::Mutex<linked_list_allocator::Heap> =
    spin::Mutex::new(linked_list_allocator::Heap::empty());

const EARLY_ALLOCATION_SIZE: usize = 1024 * 1024 * 2;
static mut EARLY_ALLOCATION_AREA: [u8; EARLY_ALLOCATION_SIZE] = [0; EARLY_ALLOCATION_SIZE];
static EARLY_ALLOCATION_PTR: AtomicUsize = AtomicUsize::new(0);
pub struct SafeZoneAllocator(spin::mutex::Mutex<ZoneAllocator<'static>>);

pub fn init(kmm: &'static KernelMemoryManager) {
    unsafe {
        PAGER.hookup_kernel_memory_manager(kmm);
        PAGER.extend_huge_heap(2 * 1024 * 1024);
        let slice =
            core::slice::from_raw_parts_mut(PAGER.huge_heap_start as *mut u8, 2 * 1024 * 1024);
        LL_BACKUP_ALLOCATOR.lock().init_from_slice(transmute(slice));
    }
}

unsafe impl GlobalAlloc for SafeZoneAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !PAGER.is_ready() {
            let start = EARLY_ALLOCATION_PTR.load(Ordering::SeqCst);
            let start = crate::utils::align(start, layout.align());
            EARLY_ALLOCATION_PTR.store(start + layout.size(), Ordering::SeqCst);
            if start + layout.size() >= EARLY_ALLOCATION_SIZE {
                panic!("out of early memory");
            }
            return EARLY_ALLOCATION_AREA.as_mut_ptr().add(start);
        }
        match layout.size() {
            HeapPager::BASE_PAGE_SIZE => {
                PAGER.allocate_page().expect("failed to allocate heap page") as *mut _ as *mut u8
            }
            HeapPager::LARGE_PAGE_SIZE => PAGER
                .allocate_large_page()
                .expect("failed to allocate large heap page")
                as *mut _ as *mut u8,
            0..=ZoneAllocator::MAX_ALLOC_SIZE => {
                let mut zone = self.0.lock();
                match zone.allocate(layout) {
                    Ok(nptr) => nptr.as_ptr(),
                    Err(AllocationError::OutOfMemory) => {
                        if layout.size() <= ZoneAllocator::MAX_BASE_ALLOC_SIZE {
                            PAGER.allocate_page().map_or(ptr::null_mut(), |page| {
                                zone.refill(layout, page)
                                    .expect("failed to refill zone allocator");
                                zone.allocate(layout)
                                    .expect("allocation failed after refill")
                                    .as_ptr()
                            })
                        } else {
                            PAGER.allocate_large_page().map_or(ptr::null_mut(), |page| {
                                zone.refill_large(layout, page)
                                    .expect("failed to refill zone allocator");
                                zone.allocate(layout)
                                    .expect("allocation failed after refill")
                                    .as_ptr()
                            })
                        }
                    }
                    Err(AllocationError::InvalidLayout) => {
                        panic!("cannot allocate this size {:?}", layout)
                    }
                }
            }
            _ => {
                let mut allocator = LL_BACKUP_ALLOCATOR.lock();
                match allocator.allocate_first_fit(layout) {
                    Ok(ptr) => ptr.as_ptr(),
                    Err(_) => {
                        let len = (layout.align() + layout.size()) * 2;
                        logln!("requesting {} bytes from huge allocator", len);
                        let len = PAGER.extend_huge_heap(len);
                        logln!("now {} bytes from huge allocator", len);
                        allocator.extend(len);
                        allocator
                            .allocate_first_fit(layout)
                            .expect("failed to allocate from huge heap after extending")
                            .as_ptr()
                    }
                }
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if !PAGER.is_ready() {
            /* freeing memory in early init. Sadly, we just have to leak it. */
            return;
        }
        /* TODO: handle deallocation back to the frame allocator and mapper */
        let nonnull = ptr.as_ref();
        if nonnull.is_none() {
            return;
        }
        match layout.size() {
            HeapPager::BASE_PAGE_SIZE => PAGER.dealloc_page(ptr, false),
            HeapPager::LARGE_PAGE_SIZE => PAGER.dealloc_page(ptr, true),
            0..=ZoneAllocator::MAX_ALLOC_SIZE => {
                if let Some(nptr) = NonNull::new(ptr) {
                    self.0
                        .lock()
                        .deallocate(nptr, layout)
                        .expect("failed to deallocate memory");
                }
            }
            _ => LL_BACKUP_ALLOCATOR
                .lock()
                .deallocate(NonNull::from(nonnull.unwrap()), layout),
        }
    }
}

#[global_allocator]
static SLAB_ALLOCATOR: SafeZoneAllocator =
    SafeZoneAllocator(spin::Mutex::new(ZoneAllocator::new()));
