use alloc::sync::Arc;

use crate::memory::frame::{self, Frame, PhysicalFrameFlags};

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
}
