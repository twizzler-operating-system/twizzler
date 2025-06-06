use std::{mem::MaybeUninit, ops::RangeBounds, alloc::{AllocError, Layout}};


fn do_alloc<A: Allocator>(
    &mut self,
    alloc: &A,
    layout: Layout,
    tx: &TxObject<()>,
) -> crate::tx::Result<RefMut<T>> {
    let new_alloc = alloc.alloc_tx(layout, tx)?;

    Ok(unsafe {new_alloc.cast::<T>().resolve().owned().mutable()})
}