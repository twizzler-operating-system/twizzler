use std::marker::PhantomData;

use twizzler_abi::object::ObjID;
use twizzler_rt_abi::object::MapFlags;

use super::Ref;
use crate::object::RawObject;

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
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(self.id(), MapFlags::READ | MapFlags::WRITE)
                .unwrap();
        let ptr = handle.lea(self.offset() as usize, size_of::<T>()).unwrap();
        Ref::from_handle(handle, ptr.cast())
    }

    pub fn is_null(&self) -> bool {
        self.id.raw() == 0
    }

    pub fn id(&self) -> ObjID {
        self.id
    }

    pub fn offset(&self) -> u64 {
        self.offset
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
