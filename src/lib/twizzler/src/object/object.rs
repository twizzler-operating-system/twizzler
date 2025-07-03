use std::marker::PhantomData;

use twizzler_abi::object::ObjID;
use twizzler_rt_abi::{
    object::{MapFlags, ObjectHandle},
    Result,
};

use super::{MutObject, RawObject, TxObject, TypedObject};
use crate::{marker::BaseType, ptr::Ref, util::maybe_remap};

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
    /// Start a transaction on this object, turning this object into a transaction object handle.
    ///
    /// # Example
    /// ```
    /// # use twizzler::object::ObjectBuilder;
    ///
    /// let obj = ObjectBuilder::new().build(12u32).unwrap();
    /// let tx_obj = obj.into_tx().unwrap();
    /// tx_obj.base_mut() += 1;
    /// ```
    pub fn into_tx(self) -> Result<TxObject<Base>> {
        TxObject::new(self)
    }

    /// Start a transaction on this object, creating a new transaction object handle.
    ///
    /// # Example
    /// ```
    /// # use twizzler::object::ObjectBuilder;
    ///
    /// let obj = ObjectBuilder::new().build(12u32).unwrap();
    /// let tx_obj = obj.as_tx().unwrap();
    /// tx_obj.base_mut() += 1;
    /// ```
    pub fn as_tx(&self) -> Result<TxObject<Base>> {
        TxObject::new(self.clone())
    }

    /// Perform a transaction on this object, within the provided closure.
    ///
    /// # Example
    /// ```
    /// # use twizzler::object::ObjectBuilder;
    ///
    /// let obj = ObjectBuilderrstarst::new().build(12u32).unwrap();
    /// obj.with_tx(|tx| tx.base_mut() += 1).unwrap();
    /// ```
    pub fn with_tx<R>(&mut self, f: impl FnOnce(&mut TxObject<Base>) -> Result<R>) -> Result<R> {
        let mut tx = self.as_tx()?;
        let r = f(&mut tx)?;
        let _ = self
            .update()
            .inspect_err(|e| tracing::warn!("failed to update {} on with_tx: {}", self.id(), e));
        Ok(r)
    }

    /// Create a new mutable object handle from this object.
    ///
    /// # Safety
    /// The caller must ensure that the underlying object is not changed
    /// outside of this mapping.
    pub unsafe fn as_mut(&self) -> Result<MutObject<Base>> {
        let (handle, _) = maybe_remap(self.handle().clone(), core::ptr::null_mut::<()>());
        Ok(unsafe { MutObject::from_handle_unchecked(handle) })
    }

    /// Create a new mutable object handle from this object.
    ///
    /// # Safety
    /// The caller must ensure that the underlying object is not changed
    /// outside of this mapping.
    pub unsafe fn into_mut(self) -> Result<MutObject<Base>> {
        let (handle, _) = maybe_remap(self.into_handle(), core::ptr::null_mut::<()>());
        Ok(unsafe { MutObject::from_handle_unchecked(handle) })
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

    /// Open a new object from its ID.
    ///
    /// The provided map flags must contain at least READ, and for stable
    /// read maps, INDIRECT. For writes, add WRITE and PERSIST.
    ///
    /// This function checks the underlying fingerprint of the base type against
    /// the stored value and fails on mismatch to ensure type safety.
    pub fn map(id: ObjID, flags: MapFlags) -> Result<Self> {
        // TODO: check base fingerprint
        let handle = twizzler_rt_abi::object::twz_rt_map_object(id, flags)?;
        tracing::debug!("map: {} {:?} => {:?}", id, flags, handle.start());
        Self::from_handle(handle)
    }

    /// Open a new object from its ID without checking the underlying fingerprint.
    ///
    /// # Safety
    /// This function is unsafe because it does not check the underlying fingerprint
    /// of the base type against the stored value. Use with caution.
    pub unsafe fn map_unchecked(id: ObjID, flags: MapFlags) -> Result<Self> {
        let handle = twizzler_rt_abi::object::twz_rt_map_object(id, flags)?;
        unsafe { Ok(Self::from_handle_unchecked(handle)) }
    }

    /// Return the ID of the object.
    pub fn id(&self) -> ObjID {
        self.handle.id()
    }

    /// Update the underlying mapping of the object. This invalidates all references to
    /// the object (hence why it takes &mut self).
    pub fn update(&mut self) -> Result<()> {
        twizzler_rt_abi::object::twz_rt_update_handle(&mut self.handle)
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
