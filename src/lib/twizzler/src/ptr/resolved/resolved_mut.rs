use std::{
    borrow::{Borrow, BorrowMut},
    marker::PhantomData,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::{LazyHandle, Ref};
use crate::{object::RawObject, ptr::GlobalPtr, util::maybe_remap};

pub struct RefMut<'obj, T> {
    ptr: *mut T,
    pub(super) lazy_handle: LazyHandle<'obj>,
    _pd: PhantomData<&'obj mut T>,
}

impl<'obj, T> RefMut<'obj, T> {
    pub(super) fn new(ptr: *mut T, lazy_handle: LazyHandle<'obj>) -> Self {
        Self {
            ptr,
            lazy_handle,
            _pd: PhantomData,
        }
    }

    #[inline]
    pub fn raw(&self) -> *mut T {
        self.ptr
    }

    pub unsafe fn from_raw_parts(ptr: *mut T, handle: &'obj ObjectHandle) -> Self {
        Self::new(ptr, LazyHandle::new_borrowed(handle))
    }

    pub fn from_handle(handle: ObjectHandle, ptr: *mut T) -> Self {
        let (handle, ptr) = maybe_remap(handle, ptr);
        Self::new(ptr, LazyHandle::new_owned(handle))
    }

    #[inline]
    pub unsafe fn from_ptr(ptr: *mut T) -> Self {
        Self::new(ptr, LazyHandle::default())
    }

    #[inline]
    pub unsafe fn cast<U>(self) -> RefMut<'obj, U> {
        RefMut::new(self.ptr.cast(), self.lazy_handle)
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.lazy_handle.handle(self.ptr.cast())
    }

    pub fn offset(&self) -> u64 {
        self.handle().ptr_local(self.ptr.cast()).unwrap() as u64
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::new(self.handle().id(), self.offset())
    }

    // Note: takes ownership to avoid aliasing
    pub fn owned<'b>(self) -> RefMut<'b, T> {
        RefMut::from_handle(self.handle().clone(), self.ptr)
    }

    pub fn as_ref(&self) -> Ref<'obj, T> {
        Ref::new(self.ptr, self.lazy_handle.clone())
    }

    pub fn into_ref(self) -> Ref<'obj, T> {
        Ref::new(self.ptr, self.lazy_handle)
    }
}

impl<'a, T> RefMut<'a, MaybeUninit<T>> {
    pub fn write(self, val: T) -> RefMut<'a, T> {
        unsafe {
            let ptr = self.ptr.as_mut().unwrap_unchecked();
            ptr.write(val);
            self.cast()
        }
    }
}

impl<'obj, T: core::fmt::Debug> core::fmt::Debug for RefMut<'obj, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.deref())
    }
}

impl<'obj, T> Deref for RefMut<'obj, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'obj, T> DerefMut for RefMut<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut().unwrap_unchecked() }
    }
}

impl<'a, T> AsMut<T> for RefMut<'a, T> {
    fn as_mut(&mut self) -> &mut T {
        &mut *self
    }
}

impl<'a, T> Borrow<T> for RefMut<'a, T> {
    fn borrow(&self) -> &T {
        &*self
    }
}

impl<'a, T> BorrowMut<T> for RefMut<'a, T> {
    fn borrow_mut(&mut self) -> &mut T {
        &mut *self
    }
}

impl<'a, T> From<RefMut<'a, T>> for GlobalPtr<T> {
    fn from(value: RefMut<'a, T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

impl<'a, T> Into<ObjectHandle> for RefMut<'a, T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<'a, T> Into<ObjectHandle> for &RefMut<'a, T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<'a, T> AsRef<ObjectHandle> for RefMut<'a, T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.handle()
    }
}
