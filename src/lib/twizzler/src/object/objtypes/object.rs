use std::{marker::PhantomData, mem::MaybeUninit};

use twizzler_abi::meta::MetaInfo;
use twizzler_runtime_api::ObjectHandle;

use super::{ImmutableObject, InitializedObject, MutableObject, RawObject};
use crate::{
    marker::InPlaceCtor,
    object::{base::BaseRef, BaseType},
    tx::{TxHandle, TxResult},
};

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

    /// Convert this handle to an immutable handle.
    ///
    /// This function treats the current object handle as immutable, without creating a new
    /// immutable object. This means that this underlying handle will not have any data modified,
    /// but the underlying memory object may still change. If you want to create a new, truely
    /// immutable object, see [InitializedObject::freeze].
    pub fn immutable(self) -> ImmutableObject<Base> {
        todo!()
    }

    fn move_in_place<'a, T: InPlaceCtor>(
        &self,
        value: T::Builder,
        place: &mut MaybeUninit<T>,
        tx: impl TxHandle<'a>,
    ) -> TxResult<()> {
        T::in_place_ctor(value, place);
        Ok(())
    }
}

impl<Base: BaseType> InitializedObject for Object<Base> {
    type Base = Base;

    fn base(&self) -> BaseRef<'_, Self::Base> {
        todo!()
    }

    fn meta(&self) -> &MetaInfo {
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
