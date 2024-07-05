use std::marker::{PhantomData, PhantomPinned};

use twizzler_runtime_api::ObjID;

#[repr(transparent)]
pub struct InvPtr<T> {
    bits: u64,
    _pd: PhantomData<*const T>,
    _pp: PhantomPinned,
}

impl<T> InvPtr<T> {
    pub fn set(&mut self, builder: InvPtrBuilder<T>) {
        todo!()
    }

    pub fn raw(&self) -> u64 {
        self.bits
    }
}

pub struct InvPtrBuilder<T> {
    id: ObjID,
    offset: u64,
    _pd: PhantomData<*const T>,
}

impl<T> InvPtrBuilder<T> {
    pub unsafe fn new_id(id: ObjID, offset: u64) -> Self {
        Self {
            id,
            offset,
            _pd: PhantomData,
        }
    }
}
