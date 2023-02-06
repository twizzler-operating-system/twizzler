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

use core::{
    intrinsics::size_of,
    mem::transmute,
    sync::atomic::{AtomicU32, Ordering},
};

use crate::once::Once;
use alloc::vec::Vec;
use intrusive_collections::{intrusive_adapter, LinkedList, LinkedListLink};

use crate::arch::memory::phys_to_virt;
use crate::spinlock::Spinlock;

use super::{MemoryRegion, MemoryRegionKind, PhysAddr};

pub type FrameRef = &'static Frame;

#[doc(hidden)]
struct AllocationRegion {
    start: PhysAddr,
    next_for_init: PhysAddr,
    pages: usize,
    zeroed: LinkedList<FrameAdapter>,
    non_zeroed: LinkedList<FrameAdapter>,
    frame_array_ptr: *mut Frame,
    frame_array_len: usize,
}

unsafe impl Send for AllocationRegion {}

impl AllocationRegion {
    fn frame_array(&self) -> &[Frame] {
        unsafe { core::slice::from_raw_parts(self.frame_array_ptr, self.frame_array_len) }
    }

    fn frame_array_mut(&mut self) -> &mut [Frame] {
        unsafe { core::slice::from_raw_parts_mut(self.frame_array_ptr, self.frame_array_len) }
    }

    fn contains(&self, pa: PhysAddr) -> bool {
        pa >= self.start && pa < (self.start + self.pages * 0x1000) // TODO: arch-dep
    }

    fn get_frame(&self, pa: PhysAddr) -> Option<FrameRef> {
        if !self.contains(pa) {
            return None;
        }
        let index = (pa - self.start) / 0x1000; // TODO: arch-dep
        assert!((index as usize) < self.frame_array_len);
        let frame = &self.frame_array()[index as usize];
        Some(unsafe { transmute(frame) })
    }

    fn get_frame_mut(&mut self, pa: PhysAddr) -> Option<&'static mut Frame> {
        if !self.contains(pa) {
            return None;
        }
        let index = (pa - self.start) / 0x1000; // TODO: arch-dep
        assert!((index as usize) < self.frame_array_len);
        let frame = &mut self.frame_array_mut()[index as usize];
        Some(unsafe { transmute(frame) })
    }

    fn admit_one(&mut self) -> bool {
        let next = self.next_for_init;
        if !self.contains(next) {
            return false;
        }
        self.next_for_init += 0x1000usize; // TODO: arch-dep

        // Unwrap-Ok: we know this address is in this region already
        let frame = self.get_frame_mut(next).unwrap();
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
        let start = m.start.align_up(0x1000u64);
        let length = m.length - (start.as_u64() - m.start.as_u64()) as usize;
        let nr_pages = length / 0x1000; // TODO: arch-dep
        if nr_pages <= 1 {
            return None;
        }
        let frame_array_len = size_of::<Frame>() * nr_pages;
        let array_pages = ((frame_array_len - 1) / 0x1000) + 1;
        if array_pages >= nr_pages {
            return None;
        }

        let frame_array_ptr = phys_to_virt(start).as_mut_ptr();

        let mut this = Self {
            start: start + array_pages * 0x1000,         // TODO: arch-dep
            next_for_init: start + array_pages * 0x1000, // TODO: arch-dep
            pages: nr_pages - array_pages,
            zeroed: LinkedList::new(FrameAdapter::NEW),
            non_zeroed: LinkedList::new(FrameAdapter::NEW),
            frame_array_ptr,
            frame_array_len,
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
    flags: PhysicalFrameFlags,
    lock: AtomicU32,
    link: LinkedListLink,
}
intrusive_adapter!(FrameAdapter = &'static Frame: Frame { link: LinkedListLink });

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl core::fmt::Debug for Frame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let this = self.lock();
        let r = f
            .debug_struct("Frame")
            .field("pa", &this.pa)
            .field("flags", &this.flags)
            .finish();
        this.unlock();
        r
    }
}

pub fn lock_two_frames<'a, 'b>(a: &'a Frame, b: &'b Frame) -> (&'a mut Frame, &'a mut Frame)
where
    'b: 'a,
{
    let a_val = a as *const Frame as usize;
    let b_val = b as *const Frame as usize;
    assert_ne!(a_val, b_val);
    if a_val > b_val {
        let lg_b = b.lock();
        let lg_a = a.lock();
        (lg_a, lg_b)
    } else {
        let lg_a = a.lock();
        let lg_b = b.lock();
        (lg_a, lg_b)
    }
}

impl Frame {
    unsafe fn reset(&mut self, pa: PhysAddr) {
        self.flags = PhysicalFrameFlags::empty();
        self.link.force_unlink();
        self.pa = pa;
        self.lock.store(0, Ordering::SeqCst);
    }

    fn lock(&self) -> &mut Self {
        while self
            .lock
            .compare_exchange_weak(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            core::hint::spin_loop();
        }
        let this = self as *const _ as *mut Self;
        unsafe { this.as_mut().unwrap() }
    }

    fn unlock(&mut self) {
        self.lock.store(0, Ordering::SeqCst);
    }

    pub fn start_address(&self) -> PhysAddr {
        self.pa
    }

    pub fn size(&self) -> usize {
        4096 //TODO: arch-dep
    }

    /// Zero a frame.
    ///
    /// This marks a frame as being zeroed and also set the underlying physical memory to zero.
    pub fn zero(&self) {
        let this = self.lock();
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };
        slice.fill(0);
        this.flags.insert(PhysicalFrameFlags::ZEROED);
        this.unlock();
    }

    /// Mark this frame as not being zeroed. Does not modify the physical memory controlled by this Frame.
    pub fn set_not_zero(&self) {
        let this = self.lock();
        this.flags.remove(PhysicalFrameFlags::ZEROED);
        this.unlock();
    }

    /// Check if this frame is marked as zeroed. Does not look at the underlying physical memory.
    pub fn is_zeroed(&self) -> bool {
        let this = self.lock();
        let z = this.flags.contains(PhysicalFrameFlags::ZEROED);
        this.unlock();
        z
    }

    pub fn set_admitted(&self) {
        let this = self.lock();
        this.flags.insert(PhysicalFrameFlags::ADMITTED);
        this.unlock();
    }

    pub fn set_free(&self) {
        let this = self.lock();
        this.flags.remove(PhysicalFrameFlags::ALLOCATED);
        this.unlock();
    }

    pub fn set_allocated(&self) {
        let this = self.lock();
        this.flags.insert(PhysicalFrameFlags::ALLOCATED);
        this.unlock();
    }

    pub fn get_flags(&self) -> PhysicalFrameFlags {
        let this = self.lock();
        let flags = this.flags;
        this.unlock();
        flags
    }

    pub fn copy_contents_from(&self, other: &Frame) {
        let (this, other) = lock_two_frames(self, other);
        if other.flags.contains(PhysicalFrameFlags::ZEROED) {
            // if both are zero, do nothing
            if this.flags.contains(PhysicalFrameFlags::ZEROED) {
                return;
            }
            // if other is zero and we aren't, just zero instead of copy
            let virt = phys_to_virt(self.pa);
            let ptr: *mut u8 = virt.as_mut_ptr();
            let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };
            slice.fill(0);
            this.flags.insert(PhysicalFrameFlags::ZEROED);
            return;
        }

        this.flags.remove(PhysicalFrameFlags::ZEROED);
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };

        let othervirt = phys_to_virt(other.pa);
        let otherptr: *mut u8 = othervirt.as_mut_ptr();
        let otherslice = unsafe { core::slice::from_raw_parts_mut(otherptr, self.size()) };

        slice.copy_from_slice(otherslice);
        this.unlock();
        other.unlock();
    }

    pub fn copy_contents_from_physaddr(&self, other: PhysAddr) {
        let this = self.lock();
        this.flags.remove(PhysicalFrameFlags::ZEROED);
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, self.size()) };

        let othervirt = phys_to_virt(other);
        let otherptr: *mut u8 = othervirt.as_mut_ptr();
        let otherslice = unsafe { core::slice::from_raw_parts_mut(otherptr, self.size()) };

        slice.copy_from_slice(otherslice);
        this.unlock();
    }
}

bitflags::bitflags! {
    /// Flags to control the state of a physical frame. Also used by the alloc functions to indicate
    /// what kind of physical frame is being requested.
    pub struct PhysicalFrameFlags: u32 {
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

/// Initialize the global physical frame allocator.
/// # Arguments
///  * `regions`: An array of memory regions passed from the boot info system.
pub fn init(regions: &[MemoryRegion]) {
    let pfa = PhysicalFrameAllocator::new(regions);
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
