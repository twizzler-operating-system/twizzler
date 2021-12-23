use core::ops::Add;

use alloc::vec::Vec;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

use crate::arch::memory::phys_to_virt;

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

#[repr(C)]
struct FreeListNode {
    next: *mut FreeListNode,
    pages: [PhysAddr; 0x1000 / 8 - 1], //TODO: arch-dep
}

const MAX_PER_PAGE: usize = 0x1000 / 8 - 1;

struct PageFreeList {
    start: *mut FreeListNode,
    index: usize,
}

impl PageFreeList {
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
        if self.index == MAX_PER_PAGE {
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
}

pub struct PhysicalFrameAllocator {
    zeroed: PageFreeList,
    non_zeroed: PageFreeList,
    regions: Vec<AllocationRegion>,
    region_idx: usize,
}

pub struct Frame {
    pa: PhysAddr,
    flags: PhysicalFrameFlags,
}

impl Frame {
    fn new(pa: PhysAddr, flags: PhysicalFrameFlags) -> Self {
        Self { pa, flags }
    }

    fn zero(&mut self) {
        let virt = phys_to_virt(self.pa);
        let ptr: *mut u8 = virt.as_mut_ptr();
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, 0x1000) };
        slice.fill(0);
        self.flags.insert(PhysicalFrameFlags::ZEROED);
    }

    fn set_not_zero(&mut self) {
        self.flags.remove(PhysicalFrameFlags::ZEROED);
    }
}

bitflags::bitflags! {
    pub struct PhysicalFrameFlags: u32 {
        const ZEROED = 1;
    }
}

impl PhysicalFrameAllocator {
    fn fallback_alloc(&mut self) -> PhysAddr {
        if let Some(pa) = self.regions[self.region_idx].take() {
            pa
        } else {
            self.region_idx += 1;
            self.fallback_alloc()
        }
    }

    pub fn alloc(&mut self, flags: PhysicalFrameFlags) -> Frame {
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

        let mut frame = Frame::new(frame, PhysicalFrameFlags::empty());
        if maybe_needs_zero && flags.contains(PhysicalFrameFlags::ZEROED) {
            frame.zero();
        }
        /* TODO: try to use the MMU to detect if a page is actually ever written to or not */
        frame.set_not_zero();

        frame
    }

    pub fn free(&mut self, frame: Frame) {
        if frame.flags.contains(PhysicalFrameFlags::ZEROED) {
            self.zeroed.push(frame.pa);
        } else {
            self.non_zeroed.push(frame.pa);
        }
    }
}
