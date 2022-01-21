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

    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.as_virtaddr().as_ptr(), self.frame.size()) }
    }

    pub fn as_mut_slice(&self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(self.as_virtaddr().as_mut_ptr(), self.frame.size())
        }
    }

    pub fn physical_address(&self) -> PhysAddr {
        self.frame.start_address()
    }

    pub fn copy_page(&self) -> Self {
        let mut new_frame = frame::alloc_frame(PhysicalFrameFlags::empty());
        new_frame.copy_contents_from(&self.frame);
        Self {
            frame: new_frame,
            count: 1,
        }
    }
}
