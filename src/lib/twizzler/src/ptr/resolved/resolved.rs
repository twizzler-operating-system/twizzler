use std::{borrow::Borrow, marker::PhantomData, ops::Deref};

use twizzler_rt_abi::object::ObjectHandle;

use super::{LazyHandle, RefMut};
use crate::{
    object::{MutObject, RawObject, TxObject},
    ptr::{GlobalPtr, TxRef},
    util::maybe_remap,
};

pub struct Ref<'obj, T> {
    ptr: *const T,
    pub(super) lazy_handle: LazyHandle<'obj>,
    _pd: PhantomData<&'obj T>,
}

impl<'obj, T> Ref<'obj, T> {
    pub(super) fn new(ptr: *const T, lazy_handle: LazyHandle<'obj>) -> Self {
        Self {
            ptr,
            lazy_handle,
            _pd: PhantomData,
        }
    }

    #[inline]
    pub fn raw(&self) -> *const T {
        self.ptr
    }

    #[inline]
    pub fn offset(&self) -> u64 {
        self.handle().ptr_local(self.ptr.cast()).unwrap() as u64
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.lazy_handle.handle(self.ptr.cast())
    }

    #[inline]
    pub unsafe fn from_raw_parts(ptr: *const T, handle: &'obj ObjectHandle) -> Self {
        Self::new(ptr, LazyHandle::new_borrowed(handle))
    }

    #[inline]
    pub unsafe fn from_ptr(ptr: *const T) -> Self {
        Self::new(ptr, LazyHandle::default())
    }

    #[inline]
    pub unsafe fn cast<U>(self) -> Ref<'obj, U> {
        Ref::new(self.ptr.cast(), self.lazy_handle)
    }

    #[inline]
    unsafe fn mutable_to(self, ptr: *mut T) -> RefMut<'obj, T> {
        RefMut::from_handle(self.handle().clone(), ptr)
    }

    #[inline]
    pub unsafe fn into_mut(self) -> RefMut<'obj, T> {
        let ptr = self.ptr as *mut T;
        self.mutable_to(ptr)
    }

    #[inline]
    pub unsafe fn as_mut(&self) -> RefMut<'obj, T> {
        let ptr = self.ptr as *mut T;
        RefMut::from_handle(self.handle().clone(), ptr)
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::new(self.handle().id(), self.offset())
    }

    pub fn owned<'b>(&self) -> Ref<'b, T> {
        Ref::from_handle(self.handle().clone(), self.ptr)
    }

    pub fn from_handle(handle: ObjectHandle, ptr: *const T) -> Self {
        Self::new(ptr, LazyHandle::new_owned(handle))
    }

    pub fn into_tx(self) -> crate::Result<TxRef<T>> {
        self.as_tx()
    }

    pub fn as_tx(&self) -> crate::Result<TxRef<T>> {
        let (handle, ptr) = maybe_remap(self.handle().clone(), self.ptr as *mut T);
        let mo = unsafe { MutObject::<()>::from_handle_unchecked(handle) };
        let tx = unsafe { TxObject::from_mut_object(mo) };
        Ok(unsafe { TxRef::from_raw_parts(tx, ptr) })
    }

    pub unsafe fn add(self, offset: usize) -> Self {
        Self::new(self.ptr.add(offset), self.lazy_handle)
    }

    pub unsafe fn byte_add(self, offset: usize) -> Self {
        Self::new(self.ptr.byte_add(offset), self.lazy_handle)
    }
}

impl<'obj, T: core::fmt::Debug> core::fmt::Debug for Ref<'obj, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.deref())
    }
}

impl<'a, T> From<Ref<'a, T>> for GlobalPtr<T> {
    fn from(value: Ref<'a, T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

impl<'obj, T> Deref for Ref<'obj, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'a, T> Into<ObjectHandle> for Ref<'a, T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<'a, T> Into<ObjectHandle> for &Ref<'a, T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<'a, T> AsRef<ObjectHandle> for Ref<'a, T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.handle()
    }
}

impl<'a, T> Borrow<T> for Ref<'a, T> {
    fn borrow(&self) -> &T {
        &*self
    }
}
