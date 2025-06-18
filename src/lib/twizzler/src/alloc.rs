use std::alloc::{AllocError, Layout};

use crate::ptr::GlobalPtr;

pub mod arena;
mod global;
pub mod invbox;

pub use global::OwnedGlobalPtr;

pub trait Allocator: Clone {
    fn alloc(&self, layout: Layout) -> Result<GlobalPtr<u8>, AllocError>;
    unsafe fn dealloc(&self, ptr: GlobalPtr<u8>, layout: Layout);
    fn realloc(
        &self,
        ptr: GlobalPtr<u8>,
        layout: Layout,
        newsize: usize,
    ) -> Result<GlobalPtr<u8>, AllocError> {
        let new_layout =
            Layout::from_size_align(newsize, layout.align()).map_err(|_| AllocError)?;

        let new_alloc = self.alloc(new_layout)?;
        unsafe {
            if !ptr.is_null() {
                let new_res = new_alloc.resolve().mutable();
                let old_res = ptr.resolve();
                let copy_len = std::cmp::min(layout.size(), new_layout.size());
                new_res.raw().copy_from(old_res.raw(), copy_len);
            }
        }
        Ok(new_alloc)
    }
}

pub trait SingleObjectAllocator {}
