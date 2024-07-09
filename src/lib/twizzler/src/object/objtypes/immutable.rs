use std::marker::PhantomData;

use twizzler_abi::meta::MetaInfo;
use twizzler_runtime_api::ObjectHandle;

use super::{InitializedObject, Object, RawObject};
use crate::object::{base::BaseRef, BaseType};

pub struct ImmutableObject<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*const Base>,
}

impl<Base: BaseType> ImmutableObject<Base> {
    pub fn object(&self) -> Object<Base> {
        todo!()
    }
}

impl<Base: BaseType> InitializedObject for ImmutableObject<Base> {
    type Base = Base;

    fn base(&self) -> BaseRef<'_, Self::Base> {
        todo!()
    }

    fn meta(&self) -> MetaInfo {
        todo!()
    }

    fn freeze(&self) -> Result<ImmutableObject<Self::Base>, ()> {
        todo!()
    }
}

impl<Base: BaseType> RawObject for ImmutableObject<Base> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

impl<Base: BaseType> Into<ObjectHandle> for ImmutableObject<Base> {
    fn into(self) -> ObjectHandle {
        self.handle
    }
}

#[derive(Copy, Clone, Debug)]
pub struct IsMutable;

impl<Base: BaseType> TryFrom<ObjectHandle> for ImmutableObject<Base> {
    type Error = IsMutable;

    fn try_from(value: ObjectHandle) -> Result<Self, Self::Error> {
        todo!()
    }
}
