use std::marker::PhantomData;

use twizzler_runtime_api::ObjID;

pub struct GlobalPtr<T> {
    id: ObjID,
    offset: u64,
    _pd: PhantomData<*const T>,
}

impl<T> GlobalPtr<T> {
    pub const fn new(id: ObjID, offset: u64) -> Self {
        Self {
            id,
            offset,
            _pd: PhantomData,
        }
    }

    pub const fn id(&self) -> ObjID {
        self.id
    }

    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub const fn cast<U>(&self) -> GlobalPtr<U> {
        GlobalPtr::new(self.id, self.offset)
    }
}

// Safety: These are the standard library rules for references (https://doc.rust-lang.org/std/primitive.reference.html).
unsafe impl<T: Sync> Sync for GlobalPtr<T> {}
unsafe impl<T: Sync> Send for GlobalPtr<T> {}
