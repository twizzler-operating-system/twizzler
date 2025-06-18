use std::marker::PhantomData;

use twizzler_abi::object::ObjID;
use twizzler_rt_abi::{
    object::{MapFlags, ObjectHandle},
    Result,
};

use super::{MutObject, RawObject, TypedObject};
use crate::{marker::BaseType, ptr::Ref, tx::TxObject};

pub struct Object<Base> {
    handle: ObjectHandle,
    _pd: PhantomData<*const Base>,
}

unsafe impl<Base> Sync for Object<Base> {}
unsafe impl<Base> Send for Object<Base> {}

impl<B> Clone for Object<B> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle.clone(),
            _pd: PhantomData,
        }
    }
}

impl<Base> Object<Base> {
    pub fn into_tx(self) -> Result<TxObject<Base>> {
        TxObject::new(self)
    }

    pub fn as_tx(&self) -> Result<TxObject<Base>> {
        TxObject::new(self.clone())
    }

    pub fn with_tx<R>(&mut self, f: impl FnOnce(&mut TxObject<Base>) -> Result<R>) -> Result<R> {
        let mut tx = self.as_tx()?;
        f(&mut tx)
    }

    pub unsafe fn as_mut(&self) -> Result<MutObject<Base>> {
        Ok(unsafe { MutObject::from_handle_unchecked(self.handle.clone()) })
    }

    pub unsafe fn from_handle_unchecked(handle: ObjectHandle) -> Self {
        Self {
            handle,
            _pd: PhantomData,
        }
    }

    pub fn from_handle(handle: ObjectHandle) -> Result<Self> {
        // TODO: check base fingerprint
        unsafe { Ok(Self::from_handle_unchecked(handle)) }
    }

    pub fn into_handle(self) -> ObjectHandle {
        self.handle
    }

    pub unsafe fn cast<U>(self) -> Object<U> {
        Object {
            handle: self.handle,
            _pd: PhantomData,
        }
    }

    pub fn map(id: ObjID, flags: MapFlags) -> Result<Self> {
        // TODO: check base fingerprint
        let handle = twizzler_rt_abi::object::twz_rt_map_object(id, flags)?;
        tracing::debug!("map: {} {:?} => {:?}", id, flags, handle.start());
        Self::from_handle(handle)
    }

    pub fn update(self) -> Result<Self> {
        let id = self.id();
        let flags = self.handle().map_flags();
        drop(self);

        Self::map(id, flags)
    }

    pub unsafe fn map_unchecked(id: ObjID, flags: MapFlags) -> Result<Self> {
        let handle = twizzler_rt_abi::object::twz_rt_map_object(id, flags)?;
        unsafe { Ok(Self::from_handle_unchecked(handle)) }
    }

    pub fn id(&self) -> ObjID {
        self.handle.id()
    }
}

impl<Base> RawObject for Object<Base> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

impl<Base: BaseType> TypedObject for Object<Base> {
    type Base = Base;

    fn base_ref(&self) -> Ref<'_, Self::Base> {
        let base = self.base_ptr();
        unsafe { Ref::from_raw_parts(base, self.handle()) }
    }

    #[inline]
    fn base(&self) -> &Self::Base {
        unsafe { self.base_ptr::<Self::Base>().as_ref().unwrap_unchecked() }
    }
}

impl<T> AsRef<ObjectHandle> for Object<T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.handle()
    }
}
