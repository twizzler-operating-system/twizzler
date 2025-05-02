//! Traits and types for working with objects.

use std::marker::PhantomData;

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::{
    error::TwzError,
    object::{MapFlags, ObjectHandle},
};

use crate::{marker::BaseType, ptr::Ref, tx::TxObject};

mod builder;
mod fot;
mod meta;

pub use builder::*;
pub use fot::*;
pub use meta::*;
pub use twizzler_rt_abi::object::ObjID;

/// Operations common to structured objects.
pub trait TypedObject {
    /// The base type of this object.
    type Base: BaseType;

    /// Returns a resolved reference to the object's base.
    fn base_ref(&self) -> Ref<'_, Self::Base>;

    fn base(&self) -> &Self::Base;
}

/// Operations common to all objects, with raw pointers.
pub trait RawObject {
    /// Get the underlying runtime handle for this object.
    fn handle(&self) -> &ObjectHandle;

    /// Get the object ID.
    fn id(&self) -> ObjID {
        self.handle().id()
    }

    /// Get a const pointer to the object base.
    fn base_ptr<T>(&self) -> *const T {
        self.lea(NULLPAGE_SIZE, size_of::<T>()).unwrap().cast()
    }

    /// Get a mut pointer to the object base.
    fn base_mut_ptr<T>(&self) -> *mut T {
        self.lea_mut(NULLPAGE_SIZE, size_of::<T>()).unwrap().cast()
    }

    /// Get a const pointer to the object metadata.
    fn meta_ptr(&self) -> *const MetaInfo {
        self.handle().meta().cast()
    }

    /// Get a mut pointer to the object metadata.
    fn meta_mut_ptr(&self) -> *mut MetaInfo {
        self.handle().meta().cast()
    }

    /// Get a const pointer to a given FOT entry.
    fn fote_ptr(&self, idx: usize) -> Option<*const FotEntry> {
        let offset: isize = (1 + idx).try_into().ok()?;
        unsafe { Some((self.meta_ptr() as *const FotEntry).offset(-offset)) }
    }

    /// Get a mut pointer to a given FOT entry.
    fn fote_ptr_mut(&self, idx: usize) -> Option<*mut FotEntry> {
        let offset: isize = (1 + idx).try_into().ok()?;
        unsafe { Some((self.meta_mut_ptr() as *mut FotEntry).offset(-offset)) }
    }

    /// Get a const pointer to given range of the object.
    fn lea(&self, offset: usize, _len: usize) -> Option<*const u8> {
        Some(unsafe { self.handle().start().add(offset) as *const u8 })
    }

    /// Get a mut pointer to given range of the object.
    fn lea_mut(&self, offset: usize, _len: usize) -> Option<*mut u8> {
        Some(unsafe { self.handle().start().add(offset) as *mut u8 })
    }

    /// If the pointer is local to this object, return the offset into the object. Otherwise, return
    /// None.
    fn ptr_local(&self, ptr: *const u8) -> Option<usize> {
        if ptr.addr() >= self.handle().start().addr()
            && ptr.addr() < self.handle().start().addr() + MAX_SIZE
        {
            Some(ptr.addr() - self.handle().start().addr())
        } else {
            None
        }
    }
}

impl RawObject for ObjectHandle {
    fn handle(&self) -> &ObjectHandle {
        self
    }
}

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
    pub fn tx(self) -> crate::tx::Result<TxObject<Base>> {
        TxObject::new(self)
    }

    pub unsafe fn from_handle_unchecked(handle: ObjectHandle) -> Self {
        Self {
            handle,
            _pd: PhantomData,
        }
    }

    pub fn from_handle(handle: ObjectHandle) -> Result<Self, TwzError> {
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

    pub fn map(id: ObjID, flags: MapFlags) -> Result<Self, TwzError> {
        let handle = twizzler_rt_abi::object::twz_rt_map_object(id, flags)?;
        tracing::debug!("map: {} {:?} => {:?}", id, flags, handle.start());
        Self::from_handle(handle)
    }

    pub fn update(self) -> crate::tx::Result<Self> {
        let id = self.id();
        let flags = self.handle().map_flags();
        drop(self);

        Self::map(id, flags)
    }

    pub unsafe fn map_unchecked(id: ObjID, flags: MapFlags) -> Result<Self, TwzError> {
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
