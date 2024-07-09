use std::marker::PhantomData;

use twizzler_abi::meta::MetaInfo;
use twizzler_runtime_api::ObjectHandle;

use super::{InitializedObject, Object, RawObject};
use crate::object::{base::BaseRef, BaseType};

pub struct MutableObject<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*const Base>,
}

impl<Base: BaseType> MutableObject<Base> {
    pub fn base_mut(&mut self) -> &mut Base {
        // Safety: part of the MutableObject contract is its existence ensures that the object is
        // locked. Thus, by taking &mut self, we can ensure no one else can point to the base.
        unsafe {
            (self.base_mut_ptr() as *mut Base)
                .as_mut()
                .unwrap_unchecked()
        }
    }

    pub fn meta_mut(&mut self) -> &mut MetaInfo {
        // Safety: part of the MutableObject contract is its existence ensures that the object is
        // locked. Thus, by taking &mut self, we can ensure no one else can point to the meta data.
        unsafe {
            (self.meta_mut_ptr() as *mut MetaInfo)
                .as_mut()
                .unwrap_unchecked()
        }
    }

    pub fn release(self) -> Object<Base> {
        todo!()
    }
}

impl<Base: BaseType> Drop for MutableObject<Base> {
    fn drop(&mut self) {
        todo!()
    }
}

impl<Base: BaseType> InitializedObject for MutableObject<Base> {
    type Base = Base;

    fn base(&self) -> BaseRef<'_, Self::Base> {
        todo!()
    }

    fn meta(&self) -> MetaInfo {
        todo!()
    }

    fn freeze(&self) -> Result<super::ImmutableObject<Self::Base>, ()> {
        todo!()
    }
}

impl<Base: BaseType> RawObject for MutableObject<Base> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IsImmutable {}

impl<Base: BaseType> TryFrom<ObjectHandle> for MutableObject<Base> {
    type Error = IsImmutable;

    fn try_from(value: ObjectHandle) -> Result<Self, Self::Error> {
        todo!()
    }
}

// Doesn't implement Into<ObjectHandle> because we implement drop, and so cannot destructure.
