use std::marker::PhantomData;

use twizzler_abi::object::ObjID;

pub struct Object<T> {
    pub(crate) slot: usize,
    pub(crate) id: ObjID,
    pub(crate) _pd: PhantomData<T>,
}

impl<T> Clone for Object<T> {
    // TODO: increase slot ref count in twizzler_abi::slot
    fn clone(&self) -> Self {
        Self {
            slot: self.slot,
            id: self.id,
            _pd: self._pd,
        }
    }
}

impl<T> Object<T> {
    pub fn id(&self) -> ObjID {
        self.id
    }
}

impl<T> Drop for Object<T> {
    fn drop(&mut self) {
        twizzler_abi::slot::global_release(self.slot);
    }
}
