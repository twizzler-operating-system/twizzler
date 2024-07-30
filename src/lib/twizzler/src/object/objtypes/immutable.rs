use std::marker::PhantomData;

use twizzler_abi::meta::MetaInfo;
use twizzler_runtime_api::ObjectHandle;

use super::{InitializedObject, Object, RawObject};
use crate::{object::BaseType, ptr::ResolvedPtr};

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

    fn base(&self) -> ResolvedPtr<'_, Self::Base> {
        todo!()
    }

    fn base_ref(&self) -> &Base {
        todo!()
    }

    fn meta(&self) -> &MetaInfo {
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

impl<Base: BaseType> From<ObjectHandle> for ImmutableObject<Base> {
    fn from(value: ObjectHandle) -> Self {
        todo!()
    }
}
