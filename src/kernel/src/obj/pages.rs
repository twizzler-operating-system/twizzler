use alloc::sync::Arc;
use x86_64::{PhysAddr, VirtAddr};

use crate::{
    arch::memory::phys_to_virt,
    memory::frame::{self, Frame, PhysicalFrameFlags},
};

pub struct Page {
    frame: Frame,
    count: usize,
}

pub type PageRef = Arc<Page>;

impl Page {
    pub fn new() -> Self {
        Self {
            frame: frame::alloc_frame(PhysicalFrameFlags::ZEROED),
            count: 1,
        }
    }

    pub fn as_virtaddr(&self) -> VirtAddr {
        phys_to_virt(self.frame.start_address())
    }

    pub fn physical_address(&self) -> PhysAddr {
        self.frame.start_address()
    }
}
