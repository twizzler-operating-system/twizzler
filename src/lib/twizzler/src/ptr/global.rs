use std::marker::PhantomData;

use twizzler_abi::object::ObjID;

use super::Ref;

#[derive(Debug, Default, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// A global pointer, containing a fully qualified object ID and offset.
pub struct GlobalPtr<T> {
    id: ObjID,
    offset: u64,
    _pd: PhantomData<*const T>,
}

impl<T> GlobalPtr<T> {
    pub fn new(id: ObjID, offset: u64) -> Self {
        Self {
            id,
            offset,
            _pd: PhantomData,
        }
    }

    pub fn cast<U>(self) -> GlobalPtr<U> {
        GlobalPtr::new(self.id, self.offset)
    }

    pub unsafe fn resolve(&self) -> Ref<'_, T> {
        todo!()
    }
}

impl<T> Clone for GlobalPtr<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            offset: self.offset,
            _pd: PhantomData,
        }
    }
}

impl<T> Copy for GlobalPtr<T> {}
