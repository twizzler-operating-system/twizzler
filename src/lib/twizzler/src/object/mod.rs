use std::marker::PhantomData;

use twizzler_runtime_api::{MapError, MapFlags, ObjID, ObjectHandle};

struct Object<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*const Base>,
}

impl<Base: BaseType> Object<Base> {
    pub fn base(&self) -> &Base {
        todo!()
    }

    pub fn open(&self, id: ObjID, flags: MapFlags) -> Result<Self, MapError> {
        todo!()
    }
}

trait BaseType {}
