use std::marker::PhantomData;

use twizzler_abi::object::ObjID;

#[derive(Debug, Default, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// A global pointer, containing a fully qualified object ID and offset.
pub struct GlobalPtr<T> {
    id: ObjID,
    offset: usize,
    _pd: PhantomData<*const T>,
}

impl<T> Clone for GlobalPtr<T> {
    fn clone(&self) -> Self {
        todo!()
    }
}

impl<T> Copy for GlobalPtr<T> {}
