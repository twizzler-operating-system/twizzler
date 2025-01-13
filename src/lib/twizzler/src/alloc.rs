use std::alloc::{AllocError, Layout};

use crate::{
    ptr::{GlobalPtr, RefSlice, RefSliceMut},
    tx::TxHandle,
};

pub mod arena;
mod global;
pub mod invbox;

pub use global::OwnedGlobalPtr;

pub trait Allocator {
    fn alloc(&self, layout: Layout) -> Result<GlobalPtr<u8>, AllocError>;
    unsafe fn dealloc(&self, ptr: GlobalPtr<u8>, layout: Layout);

    fn alloc_tx(&self, layout: Layout, _tx: &impl TxHandle) -> crate::tx::Result<GlobalPtr<u8>> {
        self.alloc(layout).map_err(|e| e.into())
    }
    unsafe fn dealloc_tx(
        &self,
        ptr: GlobalPtr<u8>,
        layout: Layout,
        _tx: &impl TxHandle,
    ) -> crate::tx::Result<()> {
        self.dealloc(ptr, layout);
        Ok(())
    }

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
                let mut new_slice = RefSliceMut::from_ref(new_res, new_layout.size());
                let old_res = ptr.resolve();
                let old_slice = RefSlice::from_ref(old_res, layout.size());
                let copy_len = std::cmp::min(old_slice.len(), new_slice.len());
                new_slice.as_slice_mut()[0..copy_len]
                    .copy_from_slice(&old_slice.as_slice()[0..copy_len]);
            }
        }
        Ok(new_alloc)
    }

    fn realloc_tx(
        &self,
        ptr: GlobalPtr<u8>,
        layout: Layout,
        newsize: usize,
        _tx: &impl TxHandle,
    ) -> Result<GlobalPtr<u8>, AllocError> {
        self.realloc(ptr, layout, newsize)
    }
}

pub trait SingleObjectAllocator {}
