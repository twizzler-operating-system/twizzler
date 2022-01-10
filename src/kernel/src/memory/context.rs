use x86_64::{
    structures::paging::{FrameAllocator, Size4KiB},
    VirtAddr,
};

use crate::arch::memory::{ArchMemoryContext, MapFlags};

use super::MappingIter;
pub struct MemoryContext {
    pub arch: ArchMemoryContext,
}

impl MemoryContext {
    pub fn new(frame_allocator: &mut impl FrameAllocator<Size4KiB>) -> Option<Self> {
        Some(Self {
            arch: ArchMemoryContext::new(frame_allocator)?,
        })
    }

    pub fn current() -> Self {
        Self {
            arch: ArchMemoryContext::current_tables(),
        }
    }

    pub fn mappings_iter(&self, start: VirtAddr) -> MappingIter {
        MappingIter::new(self, start)
    }

    pub fn clone_region(
        &mut self,
        other_ctx: &MemoryContext,
        addr: VirtAddr,
        frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    ) {
        for mapping in other_ctx.mappings_iter(addr) {
            self.arch
                .map(
                    mapping.addr,
                    mapping.frame,
                    mapping.length,
                    mapping.flags | MapFlags::USER,
                    frame_allocator,
                )
                .unwrap();
        }
    }
}
