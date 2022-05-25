use std::{marker::PhantomData, sync::Arc};

use twizzler_abi::object::ObjID;

use crate::slot::Slot;

pub struct Object<T> {
    pub(crate) slot: Arc<Slot>,
    pub(crate) _pd: PhantomData<T>,
}

impl<T> Clone for Object<T> {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot.clone(),
            _pd: self._pd,
        }
    }
}

impl<T> Object<T> {
    pub fn id(&self) -> ObjID {
        self.slot.id()
    }
}

impl<Base> From<Arc<Slot>> for Object<Base> {
    fn from(s: Arc<Slot>) -> Self {
        Self {
            slot: s,
            _pd: PhantomData,
        }
    }
}
