use std::alloc::{AllocError, Layout};

use crate::{
    marker::Invariant,
    ptr::{GlobalPtr, ResolvedMutSlice, ResolvedSlice},
    tx::TxHandle,
};

pub mod arena;
pub mod pbox;

pub trait Allocator: Invariant {
    fn allocate(&self, layout: Layout) -> Result<GlobalPtr<u8>, AllocError>;
    unsafe fn deallocate(&self, ptr: GlobalPtr<u8>, layout: Layout) -> Result<(), AllocError>;
    unsafe fn resize_in_place(
        &self,
        ptr: GlobalPtr<u8>,
        layout: Layout,
        new_size: usize,
    ) -> Result<(), AllocError> {
        if new_size <= layout.size() {
            return Ok(());
        }
        Err(AllocError)
    }

    unsafe fn grow(
        &self,
        ptr: GlobalPtr<u8>,
        layout: Layout,
        additional: usize,
    ) -> Result<GlobalPtr<u8>, AllocError> {
        let new_layout = Layout::from_size_align(layout.align(), layout.size() + additional)
            .map_err(|_| AllocError)?;
        let new_alloc = self.allocate(new_layout)?;

        let old_slice =
            ResolvedSlice::from_raw_parts(ptr.resolve().map_err(|_| AllocError)?, layout.size());
        let mut new_slice = ResolvedMutSlice::from_raw_parts(
            ptr.resolve().map_err(|_| AllocError)?.into_mut(),
            layout.size(),
        );
        new_slice.copy_from_slice(&*old_slice);
        Ok(new_alloc)
    }
}

pub trait TxAllocator {
    fn allocate<'a>(
        &self,
        layout: Layout,
        tx: impl TxHandle<'a>,
    ) -> Result<GlobalPtr<u8>, AllocError>;

    unsafe fn deallocate<'a>(
        &self,
        ptr: GlobalPtr<u8>,
        layout: Layout,
        tx: impl TxHandle<'a>,
    ) -> Result<(), AllocError>;
}
