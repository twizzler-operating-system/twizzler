use std::marker::PhantomData;

use twizzler_runtime_api::ObjID;

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
