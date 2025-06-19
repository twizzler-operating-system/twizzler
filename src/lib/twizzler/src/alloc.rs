use std::alloc::{AllocError, Layout};

use crate::ptr::GlobalPtr;

pub mod arena;
mod global;
pub mod invbox;

pub use global::OwnedGlobalPtr;

/// Basic allocation trait.
pub trait Allocator: Clone {
    /// Allocate based on layout within this allocator. Returns a global pointer
    /// to the start of the allocation.
    ///
    /// Note: Using this function by itself can leak memory, particularly on failure.
    /// Users should consider using InvBox instead.
    fn alloc(&self, layout: Layout) -> Result<GlobalPtr<u8>, AllocError>;

    /// Free an allocation.
    ///
    /// # Safety
    /// Caller must ensure that the pointer is valid and was allocated by this allocator, and
    /// refers to memory that matches the provided layout.
    unsafe fn dealloc(&self, ptr: GlobalPtr<u8>, layout: Layout);

    /// Reallocate an allocation.
    ///
    /// # Safety
    /// Caller must ensure that the pointer is valid and was allocated by this allocator, and
    /// refers to memory that matches the provided layout.
    unsafe fn realloc(
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
                let new_res = new_alloc.resolve().into_mut();
                let old_res = ptr.resolve();
                let copy_len = std::cmp::min(layout.size(), new_layout.size());
                new_res.raw().copy_from(old_res.raw(), copy_len);
            }
        }
        Ok(new_alloc)
    }
}

/// Allocator ensures that all allocations will take place within one object.
pub trait SingleObjectAllocator {}
