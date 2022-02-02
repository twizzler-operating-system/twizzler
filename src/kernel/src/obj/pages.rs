use alloc::sync::Arc;
use x86_64::{PhysAddr, VirtAddr};

use crate::{
    arch::memory::phys_to_virt,
    memory::frame::{self, Frame, PhysicalFrameFlags},
};

use super::{Object, PageNumber};

#[derive(Debug)]
pub struct Page {
    frame: Frame,
}

pub type PageRef = Arc<Page>;

impl Page {
    pub fn new() -> Self {
        Self {
            frame: frame::alloc_frame(PhysicalFrameFlags::ZEROED),
        }
    }

    pub fn as_virtaddr(&self) -> VirtAddr {
        phys_to_virt(self.frame.start_address())
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.as_virtaddr().as_ptr(), self.frame.size()) }
    }

    pub unsafe fn get_mut_to_val<T>(&self, offset: usize) -> &mut T {
        /* TODO: enforce alignment and size of offset */
        /* TODO: once we start optimizing frame zeroing, we need to make the frame as non-zeroed here */
        let va = self.as_virtaddr();
        va.as_mut_ptr::<T>()
            .add(offset / core::mem::size_of::<T>())
            .as_mut()
            .unwrap()
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
        Self { frame: new_frame }
    }
}

impl Object {
    pub unsafe fn write_val_and_signal<T>(&self, offset: usize, val: T, wakeup_count: u64) {
        {
            let mut obj_page_tree = self.lock_page_tree();
            let page_number = PageNumber::from_address(VirtAddr::new(offset as u64));
            let page_offset = offset % PageNumber::PAGE_SIZE;

            if let Some((page, _)) = obj_page_tree.get_page(page_number, true) {
                let t = page.get_mut_to_val::<T>(page_offset);
                *t = val;
            } else {
                let page = Page::new();
                obj_page_tree.add_page(page_number, page);
                drop(obj_page_tree);
                self.write_val_and_signal(offset, val, wakeup_count);
                return;
            }
            drop(obj_page_tree);
        }
        self.wakeup_word(offset, wakeup_count);
    }
}
