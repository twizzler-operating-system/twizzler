//! Manage physical frames.
//!
//! On kernel initialization, the system will call into [init] in this module to pass information
//! about physical memory regions. Once that call completes, the physical frame allocator is ready
//! for use. This has to happen before any fully-bootstrapped memory manager is ready to use. Note,
//! though, that this module may have to perform memory allocation during initialization, so it'll
//! have to make use of the bootstrap memory allocator.
//!
//! Physical frames are physical pages of memory, whose size depends on the architecture compiled
//! for. A given physical frame can either be zeroed (that is, the physical memory the frame refers
//! to contains only zeros), or it can be indeterminate. This distinction is maintained because it's
//! common that we need to allocate zero pages AND pages that will be immediately overwritten. Upon
//! allocation, the caller can request a zeroed frame or an indeterminate frame. The allocator will
//! try to reserve known-zero frames for allocations that request them.
//!
//! Allocation returns a [FrameRef], which is a static-lifetime reference to a [Frame]. The [Frame]
//! is a bit of metadata associated with each physical frame in the system. One can efficiently get
//! the [FrameRef] given a physical address, and vice versa.
//!
//! Note: this code is somewhat cursed, since it needs to do a bunch of funky low-level memory
//! management without ever triggering the memory manager (can't allocate memory, since that could
//! recurse or deadlock), and we'll need the ability to store sets of pages without allocating memory
//! outside of this module as well, hence the intrusive linked list design. Additionally, the kernel
//! needs to be able to access frame data from possibly any CPU, so the whole type must be both Sync
//! and Send. This would be easy with the lock-around-inner trick, but this plays badly with the
//! intrusive list, and so we do some cursed manual locking to ensure write isolation.
//!
//! Note: This code uses intrusive linked lists (a type of intrusive data structure). These are standard
//! practice in C kernels, but are rarely needed these days. An intrusive list is a list that stores the
//! list's link data inside the nodes (`struct Foo {link: Link, ...}`) as opposed to storing the objects in
//! the list (`struct ListItem<T> {item: T, link: Link}`). They are useful here because they can form
//! arbitrary containers while ensuring no memory is allocated to store the list, something that is very
//! important inside an allocator for physical pages. For more information, see: [https://docs.rs/intrusive-collections/latest/intrusive_collections/].

use core::{
    intrinsics::size_of,
    mem::transmute,
    sync::atomic::{AtomicU8, Ordering},
};

use crate::{arch::memory::frame::FRAME_SIZE, once::Once};
use alloc::vec::Vec;
use intrusive_collections::{intrusive_adapter, LinkedList, LinkedListLink};

use crate::arch::memory::phys_to_virt;
use crate::spinlock::Spinlock;

use super::{MemoryRegion, MemoryRegionKind, PhysAddr};

pub type FrameRef = &'static Frame;
pub type FrameMutRef = &'static mut Frame;

#[doc(hidden)]
struct AllocationRegion {
    indexer: FrameIndexer,
    next_for_init: PhysAddr,
    pages: usize,
    zeroed: LinkedList<FrameAdapter>,
    non_zeroed: LinkedList<FrameAdapter>,
}

// Safety: this is needed because of the raw pointer, but the raw pointer is static for the life of the kernel.
unsafe impl Send for AllocationRegion {}

impl AllocationRegion {
    fn contains(&self, pa: PhysAddr) -> bool {
        self.indexer.contains(pa)
    }

    fn get_frame(&self, pa: PhysAddr) -> Option<FrameRef> {
        self.indexer.get_frame(pa)
    }

    /// Get a mutable frame reference.
    ///
    /// # Safety
    /// pa must be a new frame
    unsafe fn get_frame_mut(&mut self, pa: PhysAddr) -> Option<FrameMutRef> {
        self.indexer.get_frame_mut(pa)
    }

    fn admit_one(&mut self) -> bool {
        let next = self.next_for_init;
        if !self.contains(next) {
            return false;
        }
        self.next_for_init = self.next_for_init.offset(FRAME_SIZE).unwrap();

        // Unwrap-Ok: we know this address is in this region already
        // Safety: we are allocating a new, untouched frame here
        let frame = unsafe { self.get_frame_mut(next) }.unwrap();
        // Safety: the frame can be reset since during admit_one we are the only ones with access to the frame data.
        unsafe { frame.reset(next) };
        frame.set_admitted();
        frame.set_free();
        self.non_zeroed.push_back(frame);
        true
    }

    fn free(&mut self, frame: FrameRef) {
        if !self.contains(frame.start_address()) {
            return;
        }
        frame.set_free();
        if frame.is_zeroed() {
            self.zeroed.push_back(frame);
        } else {
            self.non_zeroed.push_back(frame);
        }
    }

    fn allocate(&mut self, try_zero: bool, only_zero: bool) -> Option<FrameRef> {
        let frame = self.__do_allocate(try_zero, only_zero)?;
        assert!(!frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
        frame.set_allocated();
        Some(frame)
    }

    fn __do_allocate(&mut self, try_zero: bool, only_zero: bool) -> Option<FrameRef> {
        if only_zero {
            if let Some(f) = self.zeroed.pop_back() {
                return Some(f);
            }
            return None;
        }
        if let Some(f) = self.non_zeroed.pop_back() {
            return Some(f);
        }
        if try_zero {
            if let Some(f) = self.zeroed.pop_back() {
                return Some(f);
            }
        }
        for i in 0..16 {
            if !self.admit_one() {
                if i == 0 {
                    return None;
                }
                break;
            }
        }
        self.non_zeroed.pop_back()
    }

    fn new(m: &MemoryRegion) -> Option<Self> {
        let start = m.start.align_up(FRAME_SIZE as u64).unwrap();
        let length = m.length - (start.raw() - m.start.raw()) as usize;
        let nr_pages = length / FRAME_SIZE;
        if nr_pages <= 1 {
            return None;
        }
        let frame_array_len = size_of::<Frame>() * nr_pages;
        let array_pages = ((frame_array_len - 1) / FRAME_SIZE) + 1;
        if array_pages >= nr_pages {
            return None;
        }

        let frame_array_ptr = phys_to_virt(start).as_mut_ptr();

        let mut this = Self {
            // Safety: the pointer is to a static region of reserved memory.
            indexer: unsafe {
                FrameIndexer::new(
                    start.offset(array_pages * FRAME_SIZE).unwrap(),
                    (nr_pages - array_pages) * FRAME_SIZE,
                    frame_array_ptr,
                    frame_array_len,
                )
            },
            next_for_init: start.offset(array_pages * FRAME_SIZE).unwrap(),
            pages: nr_pages - array_pages,
            zeroed: LinkedList::new(FrameAdapter::NEW),
            non_zeroed: LinkedList::new(FrameAdapter::NEW),
        };
        for _ in 0..16 {
            this.admit_one();
        }
        Some(this)
    }
}

#[doc(hidden)]
struct PhysicalFrameAllocator {
    regions: Vec<AllocationRegion>,
    region_idx: usize,
}

/// A physical frame.
///
/// Contains a physical address and flags that indicate if the frame is zeroed or not.
pub struct Frame {
    pa: PhysAddr,
    flags: AtomicU8,
    lock: AtomicU8,
    link: LinkedListLink,
}
intrusive_adapter!(pub FrameAdapter = &'static Frame: Frame { link: LinkedListLink });

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl core::fmt::Debug for Frame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Frame")
            .field("pa", &self.pa)
            .field("flags", &self.flags.load(Ordering::SeqCst))
            .finish()
    }
}

impl Frame {
    // Safety: must only be called once, during admit_one, when the frame has not been initialized yet.
    unsafe fn reset(&mut self, pa: PhysAddr) {
        self.lock.store(0, Ordering::SeqCst);
        self.flags.store(0, Ordering::SeqCst);
        let pa_ptr = &mut self.pa as *mut _;
        *pa_ptr = pa;
        self.link.force_unlink();
        // This store acts as a release for pa as well, which synchronizes with a load in lock (or unlock), which is always called
        // at least once during allocation, so any thread that accesses a frame syncs-with this write.
        self.unlock();
    }

    pub fn with_link<R>(&self, f: impl FnOnce(&mut LinkedListLink) -> R) -> R {
        self.lock();
        let link = unsafe {
            (&self.link as *const _ as *mut LinkedListLink)
                .as_mut()
                .unwrap()
        };
        let r = f(link);
        self.unlock();
        r
    }

    fn lock(&self) {
        while self
            .lock
            .compare_exchange_weak(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.lock.store(0, Ordering::SeqCst);
    }

    /// Get the start address of the frame.
    pub fn start_address(&self) -> PhysAddr {
        self.pa
    }

    /// Get the length of the frame in bytes.
    pub fn size(&self) -> usize {
        FRAME_SIZE
    }

    /// Zero a frame.
    ///
    /// This marks a frame as being zeroed and also set the underlying physical memory to zero.
    pub fn zero(&self) {
        self.lock();
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };
        slice.fill(0);
        self.flags
            .fetch_or(PhysicalFrameFlags::ZEROED.bits(), Ordering::SeqCst);
        self.unlock();
    }

    /// Mark this frame as not being zeroed. Does not modify the physical memory controlled by this Frame.
    pub fn set_not_zero(&self) {
        self.lock();
        self.flags
            .fetch_and(!PhysicalFrameFlags::ZEROED.bits(), Ordering::SeqCst);
        self.unlock();
    }

    /// Check if this frame is marked as zeroed. Does not look at the underlying physical memory.
    pub fn is_zeroed(&self) -> bool {
        self.get_flags().contains(PhysicalFrameFlags::ZEROED)
    }

    fn set_admitted(&self) {
        self.flags
            .fetch_or(PhysicalFrameFlags::ADMITTED.bits(), Ordering::SeqCst);
    }

    fn set_free(&self) {
        self.flags
            .fetch_and(!PhysicalFrameFlags::ALLOCATED.bits(), Ordering::SeqCst);
    }

    fn set_allocated(&self) {
        self.flags
            .fetch_or(PhysicalFrameFlags::ALLOCATED.bits(), Ordering::SeqCst);
    }

    /// Get the current flags.
    pub fn get_flags(&self) -> PhysicalFrameFlags {
        PhysicalFrameFlags::from_bits_truncate(self.flags.load(Ordering::SeqCst))
    }

    /// Copy contents of one frame into another. If the other frame is marked as zeroed, copying will not happen. Both
    /// frames are locked first.
    pub fn copy_contents_from(&self, other: &Frame) {
        self.lock();
        // We don't need to lock the other frame, since if its contents aren't synchronized with this operation, it
        // could have reordered to before or after.
        if other.is_zeroed() {
            // if both are zero, do nothing
            if self.is_zeroed() {
                self.unlock();
                return;
            }
            // if other is zero and we aren't, just zero instead of copy
            let virt = phys_to_virt(self.pa);
            let ptr: *mut u8 = virt.as_mut_ptr();
            let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };
            slice.fill(0);
            self.flags
                .fetch_or(PhysicalFrameFlags::ZEROED.bits(), Ordering::SeqCst);
            self.unlock();
            return;
        }

        self.flags
            .fetch_and(!PhysicalFrameFlags::ZEROED.bits(), Ordering::SeqCst);
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };

        let othervirt = phys_to_virt(other.pa);
        let otherptr: *mut u8 = othervirt.as_mut_ptr();
        let otherslice = unsafe { core::slice::from_raw_parts_mut(otherptr, self.size()) };

        slice.copy_from_slice(otherslice);
        self.unlock();
    }

    /// Copy from another physical address into this frame.
    pub fn copy_contents_from_physaddr(&self, other: PhysAddr) {
        self.lock();
        self.flags
            .fetch_and(!PhysicalFrameFlags::ZEROED.bits(), Ordering::SeqCst);
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };

        let othervirt = phys_to_virt(other);
        let otherptr: *mut u8 = othervirt.as_mut_ptr();
        let otherslice = unsafe { core::slice::from_raw_parts_mut(otherptr, self.size()) };

        slice.copy_from_slice(otherslice);
        self.unlock();
    }
}

bitflags::bitflags! {
    /// Flags to control the state of a physical frame. Also used by the alloc functions to indicate
    /// what kind of physical frame is being requested.
    #[derive(Clone, Copy)]
    pub struct PhysicalFrameFlags: u8 {
        /// The frame is zeroed (or, allocate a zeroed frame)
        const ZEROED = 1;
        /// The frame has been allocated by the system.
        const ALLOCATED = 2;
        /// (internal) The frame has been admitted into the frame tracking system.
        const ADMITTED = 4;
    }
}

impl PhysicalFrameAllocator {
    fn new(memory_regions: &[MemoryRegion]) -> PhysicalFrameAllocator {
        Self {
            region_idx: 0,
            regions: memory_regions
                .iter()
                .filter_map(|m| {
                    if m.kind == MemoryRegionKind::UsableRam {
                        AllocationRegion::new(m)
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }

    fn alloc(&mut self, flags: PhysicalFrameFlags, fallback: bool) -> Option<FrameRef> {
        let frame = if fallback {
            Some(self.__do_alloc_fallback())
        } else {
            self.__do_alloc(flags)
        }?;
        if flags.contains(PhysicalFrameFlags::ZEROED) && !frame.is_zeroed() {
            frame.zero();
        }
        Some(frame)
    }

    fn __do_alloc_fallback(&mut self) -> FrameRef {
        // fallback
        for reg in &mut self.regions {
            let frame = reg.allocate(true, false);
            if let Some(frame) = frame {
                return frame;
            }
        }
        panic!("out of memory");
    }

    fn __do_alloc(&mut self, flags: PhysicalFrameFlags) -> Option<FrameRef> {
        let needs_zero = flags.contains(PhysicalFrameFlags::ZEROED);
        // try to find an exact match
        for reg in &mut self.regions {
            let frame = reg.allocate(false, needs_zero);
            if frame.is_some() {
                return frame;
            }
        }
        None
    }

    fn free(&mut self, frame: FrameRef) {
        for reg in &mut self.regions {
            if reg.contains(frame.start_address()) {
                reg.free(frame);
                return;
            }
        }
    }
}

#[doc(hidden)]
static PFA: Once<Spinlock<PhysicalFrameAllocator>> = Once::new();

#[derive(Clone)]
struct FrameIndexer {
    start: PhysAddr,
    len: usize,
    frame_array_ptr: *const Frame,
    frame_array_len: usize,
}

impl FrameIndexer {
    /// Build a new frame indexer.
    ///
    /// # Safety: The passed pointer and len must point to a valid section of memory reserved for the frame slice, which will last the lifetime of the kernel.
    unsafe fn new(
        start: PhysAddr,
        len: usize,
        frame_array_ptr: *const Frame,
        frame_array_len: usize,
    ) -> Self {
        Self {
            start,
            len,
            frame_array_ptr,
            frame_array_len,
        }
    }

    fn frame_array(&self) -> &[Frame] {
        unsafe { core::slice::from_raw_parts(self.frame_array_ptr, self.frame_array_len) }
    }

    fn frame_array_mut(&mut self) -> &mut [Frame] {
        unsafe {
            core::slice::from_raw_parts_mut(self.frame_array_ptr as *mut _, self.frame_array_len)
        }
    }

    fn get_frame(&self, pa: PhysAddr) -> Option<FrameRef> {
        if !self.contains(pa) {
            return None;
        }
        let index = (pa - self.start) / FRAME_SIZE;
        assert!(index < self.frame_array_len);
        let frame = &self.frame_array()[index as usize];
        // Safety: the frame array is static for the life of the kernel
        Some(unsafe { transmute(frame) })
    }

    unsafe fn get_frame_mut(&mut self, pa: PhysAddr) -> Option<FrameMutRef> {
        if !self.contains(pa) {
            return None;
        }
        let index = (pa - self.start) / FRAME_SIZE;
        assert!(index < self.frame_array_len);
        let frame = &mut self.frame_array_mut()[index as usize];
        // Safety: the frame array is static for the life of the kernel
        Some(unsafe { transmute(frame) })
    }

    fn contains(&self, pa: PhysAddr) -> bool {
        pa >= self.start && pa < (self.start.offset(self.len).unwrap())
    }
}

// Safety: this is needed because of the raw pointer, but the raw pointer is static for the life of the kernel.
unsafe impl Send for FrameIndexer {}
unsafe impl Sync for FrameIndexer {}

#[doc(hidden)]
static FI: Once<Vec<FrameIndexer>> = Once::new();

/// Initialize the global physical frame allocator.
/// # Arguments
///  * `regions`: An array of memory regions passed from the boot info system.
pub fn init(regions: &[MemoryRegion]) {
    let pfa = PhysicalFrameAllocator::new(regions);
    FI.call_once(|| pfa.regions.iter().map(|r| r.indexer.clone()).collect());
    PFA.call_once(|| Spinlock::new(pfa));
}

/// Allocate a physical frame.
///
/// The `flags` argument allows one to control if the resulting frame is
/// zeroed or not. Note that passing [PhysicalFrameFlags]::ZEROED guarantees that the returned frame
/// is zeroed, but the converse is not true.
///
/// The returned frame will have its ZEROED flag cleared. In the future, this will probably change
/// to reflect the correct state of the frame.
///
/// # Panic
/// Will panic if out of physical memory. For this reason, you probably want to use [try_alloc_frame].
///
/// # Examples
/// ```
/// let uninitialized_frame = alloc_frame(PhysicalFrameFlags::empty());
/// let zeroed_frame = alloc_frame(PhysicalFrameFlags::ZEROED);
/// ```
pub fn alloc_frame(flags: PhysicalFrameFlags) -> FrameRef {
    let mut frame = { PFA.wait().lock().alloc(flags, false) };
    if frame.is_none() {
        frame = PFA.wait().lock().alloc(flags, true);
    }
    let frame = frame.expect("out of memory");
    if flags.contains(PhysicalFrameFlags::ZEROED) {
        assert!(frame.is_zeroed());
    }
    /* TODO: try to use the MMU to detect if a page is actually ever written to or not */
    frame.set_not_zero();
    assert!(frame.get_flags().contains(PhysicalFrameFlags::ADMITTED));
    assert!(frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
    frame
}

/// Try to allocate a physical frame. The flags argument is the same as in [alloc_frame]. Returns
/// None if no physical frame is available.
pub fn try_alloc_frame(flags: PhysicalFrameFlags) -> Option<FrameRef> {
    Some(alloc_frame(flags))
}

/// Free a physical frame.
///
/// If the frame's flags indicates that it is zeroed, it will be placed on
/// the zeroed list.
pub fn free_frame(frame: FrameRef) {
    assert!(frame.get_flags().contains(PhysicalFrameFlags::ADMITTED));
    assert!(frame.get_flags().contains(PhysicalFrameFlags::ALLOCATED));
    PFA.wait().lock().free(frame);
}

/// Get a FrameRef from a physical address.
pub fn get_frame(pa: PhysAddr) -> Option<FrameRef> {
    let fi = FI.wait();
    for fi in fi {
        let f = fi.get_frame(pa);
        if f.is_some() {
            return f;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use twizzler_kernel_macros::kernel_test;

    use crate::utils::quick_random;

    use super::{alloc_frame, free_frame, get_frame, PhysicalFrameFlags};

    #[kernel_test]
    fn test_get_frame() {
        let frame = alloc_frame(PhysicalFrameFlags::empty());
        let addr = frame.start_address();
        let test_frame = get_frame(addr).unwrap();
        assert!(core::ptr::eq(frame as *const _, test_frame as *const _));
    }

    #[kernel_test]
    fn stress_test_pmm() {
        let mut stack = Vec::new();
        for _ in 0..100000 {
            let x = quick_random();
            let y = quick_random();
            let z = quick_random();
            if x % 2 == 0 && stack.len() < 1000 {
                let frame = if y % 3 == 0 {
                    alloc_frame(PhysicalFrameFlags::ZEROED)
                } else {
                    alloc_frame(PhysicalFrameFlags::empty())
                };
                if z % 5 == 0 {
                    frame.zero();
                }
                stack.push(frame);
            } else {
                if let Some(frame) = stack.pop() {
                    free_frame(frame);
                }
            }
        }
    }
}
