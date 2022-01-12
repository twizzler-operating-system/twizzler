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

use core::ops::Add;

use alloc::vec::Vec;
use spin::Once;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

use crate::arch::memory::phys_to_virt;
use crate::spinlock::Spinlock;

use super::{MemoryRegion, MemoryRegionKind};

pub struct BootFrameAllocator {
    regions: &'static [MemoryRegion],
    next: usize,
}

impl BootFrameAllocator {
    pub unsafe fn init(regions: &'static [MemoryRegion]) -> Self {
        BootFrameAllocator { regions, next: 0 }
    }

    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        let regions = self.regions.iter();
        let usable_regions = regions.filter(|r| r.kind == MemoryRegionKind::UsableRam);
        let addr_ranges =
            usable_regions.map(|r| r.start.as_u64()..(r.start.as_u64() + r.length as u64));
        let frame_addresses = addr_ranges.flat_map(|r| r.step_by(4096 /* TODO: arch-dep */));
        frame_addresses.map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        if let Some(frame) = frame {
            if frame.start_address().as_u64() == 0 {
                return self.allocate_frame();
            }
            if frame.start_address().as_u64() < 0xa0000 {
                return self.allocate_frame();
            }
        }
        frame
    }
}

#[doc(hidden)]
#[repr(C)]
struct FreeListNode {
    next: *mut FreeListNode,
    pages: [PhysAddr; 0x1000 / 8 - 1], //TODO: arch-dep
}

#[doc(hidden)]
const MAX_PER_PAGE: usize = 0x1000 / 8 - 1;

#[doc(hidden)]
struct PageFreeList {
    start: *mut FreeListNode,
    index: usize,
}

impl PageFreeList {
    fn new() -> Self {
        Self {
            start: core::ptr::null_mut(),
            index: 0,
        }
    }

    fn pop(&mut self) -> Option<(bool, PhysAddr)> {
        if self.start.is_null() {
            return None;
        }
        if self.index == 0 {
            let addr = self.start as u64;
            self.start = unsafe { &*self.start }.next;
            self.index = MAX_PER_PAGE;
            let vtop = phys_to_virt(PhysAddr::new(0)).as_u64();
            let paddr = addr - vtop;
            Some((true, PhysAddr::new(paddr)))
        } else {
            self.index -= 1;
            Some((false, unsafe { &*self.start }.pages[self.index]))
        }
    }

    fn push(&mut self, addr: PhysAddr) {
        if self.index == MAX_PER_PAGE || self.start.is_null() {
            let vaddr = phys_to_virt(addr);
            let node: &mut FreeListNode = unsafe { &mut *vaddr.as_mut_ptr() };
            node.next = self.start;
            /* TODO: we probably can avoid this assignment */
            node.pages = [PhysAddr::new(0); 0x1000 / 8 - 1];
            self.index = 0;
            self.start = node as *mut FreeListNode;
        } else {
            unsafe { &mut *self.start }.pages[self.index] = addr;
            self.index += 1;
        }
    }
}

#[doc(hidden)]
struct AllocationRegion {
    start: PhysAddr,
    pages: usize,
}

impl AllocationRegion {
    fn take(&mut self) -> Option<PhysAddr> {
        if self.pages == 0 {
            return None;
        }
        let pa = self.start;
        self.start = self.start.add(0x1000usize); //TODO: arch-dep
        self.pages -= 1;
        Some(pa)
    }

    fn new(m: &MemoryRegion) -> Self {
        let start = m.start.align_up(0x1000u64);
        let length = m.length - (start.as_u64() - m.start.as_u64()) as usize;
        Self {
            start,
            pages: length / 0x1000,
        }
    }
}

#[doc(hidden)]
struct PhysicalFrameAllocator {
    zeroed: PageFreeList,
    non_zeroed: PageFreeList,
    regions: Vec<AllocationRegion>,
    region_idx: usize,
}

/// A physical frame.
///
/// Contains a physical address and flags that indicate if the frame is zeroed or not.
pub struct Frame {
    pa: PhysAddr,
    flags: PhysicalFrameFlags,
}

impl Frame {
    fn new(pa: PhysAddr, flags: PhysicalFrameFlags) -> Self {
        Self { pa, flags }
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
    pub fn zero(&mut self) {
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, 0x1000) };
        slice.fill(0);
        self.flags.insert(PhysicalFrameFlags::ZEROED);
    }

    /// Mark this frame as not being zeroed. Does not modify the physical memory controlled by this Frame.
    pub fn set_not_zero(&mut self) {
        self.flags.remove(PhysicalFrameFlags::ZEROED);
    }

    /// Check if this frame is marked as zeroed. Does not look at the underlying physical memory.
    pub fn is_zeroed(&self) -> bool {
        self.flags.contains(PhysicalFrameFlags::ZEROED)
    }
}

bitflags::bitflags! {
    /// Flags to control the state of a physical frame. Also used by the alloc functions to indicate
    /// what kind of physical frame is being requested.
    pub struct PhysicalFrameFlags: u32 {
        /// The frame is zeroed (or, allocate a zeroed frame)
        const ZEROED = 1;
    }
}

impl PhysicalFrameAllocator {
    fn new(memory_regions: &[MemoryRegion]) -> PhysicalFrameAllocator {
        Self {
            zeroed: PageFreeList::new(),
            non_zeroed: PageFreeList::new(),
            region_idx: 0,
            regions: memory_regions
                .iter()
                .filter_map(|m| {
                    if m.kind == MemoryRegionKind::UsableRam {
                        Some(AllocationRegion::new(m))
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }
    fn fallback_alloc(&mut self) -> PhysAddr {
        if self.region_idx >= self.regions.len() {
            panic!("out of physical memory");
        }
        if let Some(pa) = self.regions[self.region_idx].take() {
            pa
        } else {
            self.region_idx += 1;
            self.fallback_alloc()
        }
    }

    fn alloc(&mut self, flags: PhysicalFrameFlags) -> Frame {
        let (primary, fallback) = if flags.contains(PhysicalFrameFlags::ZEROED) {
            (&mut self.zeroed, &mut self.non_zeroed)
        } else {
            (&mut self.non_zeroed, &mut self.zeroed)
        };

        let (maybe_needs_zero, frame) = {
            if let Some(res) = primary.pop() {
                res
            } else if let Some(res) = fallback.pop() {
                (true, res.1)
            } else {
                (true, self.fallback_alloc())
            }
        };

        Frame::new(
            frame,
            if maybe_needs_zero {
                PhysicalFrameFlags::empty()
            } else {
                PhysicalFrameFlags::ZEROED
            },
        )
    }

    fn free(&mut self, frame: Frame) {
        if frame.flags.contains(PhysicalFrameFlags::ZEROED) {
            self.zeroed.push(frame.pa);
        } else {
            self.non_zeroed.push(frame.pa);
        }
    }
}

unsafe impl Send for PageFreeList {}

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
pub fn alloc_frame(flags: PhysicalFrameFlags) -> Frame {
    let mut frame = { PFA.wait().lock().alloc(flags) };
    if !frame.flags.contains(PhysicalFrameFlags::ZEROED)
        && flags.contains(PhysicalFrameFlags::ZEROED)
    {
        frame.zero();
    }
    /* TODO: try to use the MMU to detect if a page is actually ever written to or not */
    frame.set_not_zero();
    frame
}

/// Try to allocate a physical frame. The flags argument is the same as in [alloc_frame]. Returns
/// None if no physical frame is available.
pub fn try_alloc_frame(flags: PhysicalFrameFlags) -> Option<Frame> {
    Some(alloc_frame(flags))
}

/// Free a physical frame.
///
/// If the frame's flags indicates that it is zeroed, it will be placed on
/// the zeroed list.
pub fn free_frame(frame: Frame) {
    PFA.wait().lock().free(frame);
}
