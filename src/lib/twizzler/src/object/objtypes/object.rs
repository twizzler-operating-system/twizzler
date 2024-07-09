use std::marker::PhantomData;

use twizzler_abi::meta::MetaInfo;
use twizzler_runtime_api::ObjectHandle;

use super::{ImmutableObject, InitializedObject, MutableObject, RawObject};
use crate::object::{base::BaseRef, BaseType};

pub struct Object<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*const Base>,
}

impl<Base: BaseType> Object<Base> {
    pub unsafe fn base_mut(&self) -> &mut Base {
        (self.base_mut_ptr() as *mut Base)
            .as_mut()
            .unwrap_unchecked()
    }

    // TODO: error type
    pub fn mutable(self) -> Result<MutableObject<Base>, ()> {
        todo!()
    }

    // TODO: error type
    pub fn immutable(self) -> Result<ImmutableObject<Base>, ()> {
        todo!()
    }
}

impl<Base: BaseType> InitializedObject for Object<Base> {
    type Base = Base;

    fn base(&self) -> BaseRef<'_, Self::Base> {
        todo!()
    }

    fn meta(&self) -> MetaInfo {
        // requires checking for tears (no version bumps)
        todo!()
    }

    fn freeze(&self) -> Result<ImmutableObject<Self::Base>, ()> {
        todo!()
    }
}

impl<Base: BaseType> RawObject for Object<Base> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

impl<Base: BaseType> Into<ObjectHandle> for Object<Base> {
    fn into(self) -> ObjectHandle {
        self.handle
    }
}
