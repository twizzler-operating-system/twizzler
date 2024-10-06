use std::{marker::PhantomData, pin::Pin};

use twizzler_abi::meta::MetaInfo;
use twizzler_runtime_api::ObjectHandle;

use super::{ImmutableObject, InitializedObject, Object, RawObject};
use crate::{object::BaseType, ptr::ResolvedPtr, tx::TxHandle};

pub struct MutableObject<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*mut Base>,
}

impl<Base: BaseType> MutableObject<Base> {
    pub fn base_mut(&mut self) -> Pin<&mut Base> {
        // Safety: part of the MutableObject contract is its existence ensures that the object is
        // locked. Thus, by taking &mut self, we can ensure no one else can point to the base.
        unsafe {
            Pin::new_unchecked(
                (self.base_mut_ptr().cast::<Base>())
                    .as_mut()
                    .unwrap_unchecked(),
            )
        }
    }

    pub fn meta_mut(&mut self) -> &mut MetaInfo {
        // Safety: part of the MutableObject contract is its existence ensures that the object is
        // locked. Thus, by taking &mut self, we can ensure no one else can point to the meta data.
        unsafe {
            (self.meta_mut_ptr().cast::<MetaInfo>())
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
        // TODO
    }
}

impl<Base: BaseType> InitializedObject for MutableObject<Base> {
    type Base = Base;

    fn base(&self) -> ResolvedPtr<'_, Self::Base> {
        unsafe { ResolvedPtr::new(self.base_ptr().cast()) }
    }

    fn base_ref(&self) -> &Base {
        unsafe { &*(self.base_ptr().cast()) }
    }

    fn meta(&self) -> &MetaInfo {
        unsafe { &*(self.meta_ptr().cast()) }
    }

    fn freeze(&self) -> Result<ImmutableObject<Self::Base>, ()> {
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
        // TODO: check if this is okay
        Ok(Self {
            handle: value,
            _pd: PhantomData,
        })
    }
}

impl<'a, B: BaseType> TxHandle<'a> for MutableObject<B> {
    fn tx_mut<T, E>(&self, data: *const T) -> crate::tx::TxResult<*mut T, E> {
        // TODO: check if pointer is in this object
        // TODO: ensure uniqueness of returned pointers?
        Ok(data as *mut T)
    }
}

// Doesn't implement Into<ObjectHandle> because we implement drop, and so cannot destructure.