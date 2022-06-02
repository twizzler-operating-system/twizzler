use std::{marker::PhantomData, sync::Arc};

use twizzler_abi::object::ObjID;

use crate::slot::Slot;

/// A handle for an object with base type T.
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
    /// Get the ID of this object.
    pub fn id(&self) -> ObjID {
        self.slot.id()
    }

    /// Get the slot of this object.
    pub fn slot(&self) -> &Arc<Slot> {
        &self.slot
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
