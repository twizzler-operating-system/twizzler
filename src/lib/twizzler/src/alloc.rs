use std::{
    alloc::{AllocError, Layout},
    mem::MaybeUninit,
};

use invbox::InvBox;

use crate::{marker::Invariant, ptr::GlobalPtr, tx::TxHandle};

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
}

pub trait SingleObjectAllocator {}
