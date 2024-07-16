use std::alloc::{AllocError, Layout};

use crate::{ptr::GlobalPtr, tx::TxHandle};

pub mod arena;
pub mod pbox;

pub trait Allocator {
    fn allocate(&self, layout: Layout) -> Result<GlobalPtr<u8>, AllocError>;
    unsafe fn deallocate(&self, ptr: GlobalPtr<u8>, layout: Layout) -> Result<(), AllocError>;
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
