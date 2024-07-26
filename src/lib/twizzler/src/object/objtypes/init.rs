use std::{marker::PhantomData, mem::MaybeUninit};

use twizzler_abi::meta::MetaInfo;
use twizzler_runtime_api::{MapError, MapFlags, ObjID, ObjectHandle};

use super::{ImmutableObject, Object, RawObject};
use crate::{
    object::BaseType,
    ptr::{InvPtr, ResolvedPtr},
};

pub trait InitializedObject: RawObject {
    type Base: BaseType;

    fn base(&self) -> &Self::Base;

    fn map(id: ObjID, flags: MapFlags) -> Result<Self, MapError>
    where
        Self: Sized,
    {
        todo!()
    }

    fn meta(&self) -> &MetaInfo;

    // TODO: Error type
    fn freeze(&self) -> Result<ImmutableObject<Self::Base>, ()>;

    /// Resolves an invariant pointer for this object.
    ///
    /// This function checks to ensure the passed in invariant pointer really is
    /// part of the object. It then tries to resolve the pointer according to its contents and this
    /// object's FOT. The resulting resolved pointer does NOT implement Deref, since it may not be
    /// memory safe to do so in general (we cannot prove that noone has a mutable reference).
    unsafe fn resolve<T>(&self, ptr: &InvPtr<T>) -> Result<ResolvedPtr<'_, T>, ()> {
        todo!()
    }
}

pub struct UninitializedObject<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*const MaybeUninit<Base>>,
}

impl<Base: BaseType> UninitializedObject<Base> {
    pub unsafe fn assume_init(self) -> Object<Base> {
        todo!()
    }

    pub fn init(&mut self, init: Base) -> Object<Base> {
        todo!()
    }
}

impl<Base: BaseType> RawObject for UninitializedObject<Base> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

impl<Base: BaseType> Into<ObjectHandle> for UninitializedObject<Base> {
    fn into(self) -> ObjectHandle {
        self.handle
    }
}
