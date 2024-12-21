use std::{
    alloc::{AllocError, Layout},
    mem::MaybeUninit,
};

use crate::{ptr::GlobalPtr, tx::TxHandle};

pub mod arena;
pub mod invbox;

pub trait Allocator {
    fn alloc(&self, layout: Layout) -> Result<GlobalPtr<u8>, AllocError>;
    unsafe fn dealloc(&self, ptr: GlobalPtr<u8>, layout: Layout);

    fn tx_new<T>(&self, value: T, tx: &impl TxHandle) -> crate::tx::Result<GlobalPtr<T>> {
        unsafe {
            self.tx_new_inplace(
                |place| {
                    place.write(value);
                    Ok(())
                },
                tx,
            )
        }
    }

    unsafe fn tx_new_inplace<T, F>(
        &self,
        ctor: F,
        tx: &impl TxHandle,
    ) -> crate::tx::Result<GlobalPtr<T>>
    where
        F: FnOnce(&mut MaybeUninit<T>) -> crate::tx::Result<()>,
    {
        todo!()
    }
}

pub trait SingleObjectAllocator {}
