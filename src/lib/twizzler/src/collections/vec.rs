use std::alloc::Layout;

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use crate::{
    alloc::Allocator,
    marker::{Invariant, StoreCopy},
    object::ObjectBuilder,
    ptr::InvPtr,
    tx::{Result, TxHandle},
};

pub struct Vec<T: Invariant, Alloc: Allocator> {
    len: usize,
    cap: usize,
    start: InvPtr<T>,
    alloc: Alloc,
}

impl<T: Invariant + StoreCopy, Alloc: Allocator> Vec<T, Alloc> {
    /// Push an item, abstract over the allocator. Requires a transaction handle and T to be
    /// StoreCopy, since it might move during resize.
    pub fn push_copy(&self, item: T, tx: &impl TxHandle) -> Result<()> {
        if self.len == self.cap {
            // resize via allocator
        }
        // get start slice
        // write item, tracking in tx
        let this: *mut Self = tx
            .tx_mut(self as *const _ as *const u8, size_of::<Self>())?
            .cast();
        let len = unsafe { &raw mut (*this).len };
        unsafe { *len += 1 };
        Ok(())
    }
}

struct SingleObject;

impl Allocator for SingleObject {}

impl<T: Invariant> Vec<T, SingleObject> {
    pub fn new() -> Self {
        let offset = size_of::<Self>().next_multiple_of(align_of::<T>());
        Self {
            cap: 0,
            len: 0,
            start: InvPtr::from_raw_parts(0, offset as u64),
            alloc: SingleObject,
        }
    }

    /// Push an item. T need not be StoreCopy since it will definitely be pushed to only a single
    /// object, and requries &mut self to ensure the transaction was already started.
    pub fn push(&mut self, item: T, tx: &impl TxHandle) -> Result<()> {
        if self.len == self.cap {
            if self.start.raw() as usize + size_of::<T>() * self.cap >= MAX_SIZE - NULLPAGE_SIZE {
                return Err(crate::tx::TxError::Exhausted);
            }
            self.cap += 1;
        }
        // get start slice
        // write item, tracking in tx
        self.len += 1;
        Ok(())
    }
}
