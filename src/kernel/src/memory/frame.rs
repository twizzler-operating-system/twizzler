use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

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
