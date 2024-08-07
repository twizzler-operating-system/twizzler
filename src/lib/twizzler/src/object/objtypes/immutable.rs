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
    pub fn object(self) -> Object<Base> {
        unsafe { Object::new(self.handle) }
    }
}

impl<Base: BaseType> InitializedObject for ImmutableObject<Base> {
    type Base = Base;

    fn base(&self) -> ResolvedPtr<'_, Self::Base> {
        unsafe { ResolvedPtr::new_with_handle_ref(self.base_ptr().cast(), &self.handle) }
    }

    fn base_ref(&self) -> &Base {
        unsafe { &*self.base_ptr().cast() }
    }

    fn meta(&self) -> &MetaInfo {
        unsafe { &*self.meta_ptr().cast() }
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
        Self {
            handle: value,
            _pd: PhantomData,
        }
    }
}
