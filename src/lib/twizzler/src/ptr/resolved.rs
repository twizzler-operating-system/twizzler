use std::{
    borrow::Cow,
    cell::OnceCell,
    ops::{Deref, DerefMut},
};

use twizzler_runtime_api::ObjectHandle;

#[repr(transparent)]
#[derive(Clone, Default)]
pub(crate) struct OnceHandle<'a>(OnceCell<Cow<'a, ObjectHandle>>);

impl<'a> OnceHandle<'a> {
    pub(crate) fn handle(&self, ptr: *const u8) -> &ObjectHandle {
        self.0.get_or_init(|| {
            let runtime = twizzler_runtime_api::get_runtime();
            Cow::Owned(runtime.ptr_to_handle(ptr).unwrap().0)
        })
    }

    pub(crate) fn new(handle: ObjectHandle) -> Self {
        Self(OnceCell::from(Cow::Owned(handle)))
    }
}

pub struct ResolvedPtr<'obj, T> {
    ptr: *const T,
    once_handle: OnceHandle<'obj>,
}

impl<'obj, T> Clone for ResolvedPtr<'obj, T> {
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr,
            once_handle: self.once_handle.clone(),
        }
    }
}

impl<'obj, T> ResolvedPtr<'obj, T> {
    pub(crate) unsafe fn new(ptr: *const T) -> Self {
        Self {
            ptr,
            once_handle: OnceHandle::default(),
        }
    }

    pub(crate) unsafe fn new_with_handle(ptr: *const T, handle: ObjectHandle) -> Self {
        Self {
            ptr,
            once_handle: OnceHandle::new(handle),
        }
    }

    pub unsafe fn as_mut(&'obj self) -> ResolvedMutPtr<'obj, T> {
        ResolvedMutPtr {
            handle: self.handle(),
            ptr: self.ptr as *mut T,
        }
    }

    pub unsafe fn add(self, offset: usize) -> Self {
        Self {
            ptr: self.ptr.add(offset),
            once_handle: self.once_handle,
        }
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.once_handle.handle(self.ptr as *const u8)
    }

    pub fn ptr(&self) -> *const T {
        self.ptr
    }

    pub fn owned<'a>(&self) -> ResolvedPtr<'a, T> {
        ResolvedPtr {
            ptr: self.ptr(),
            once_handle: OnceHandle::new(self.handle().clone()),
        }
    }
}

impl<'obj, T> Deref for ResolvedPtr<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

pub struct ResolvedMutPtr<'obj, T> {
    handle: &'obj ObjectHandle,
    ptr: *mut T,
}

impl<'obj, T> ResolvedMutPtr<'obj, T> {
    pub fn handle(&self) -> &ObjectHandle {
        self.handle
    }

    pub fn ptr(&self) -> *mut T {
        self.ptr
    }
}

impl<'obj, T> Deref for ResolvedMutPtr<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'obj, T> DerefMut for ResolvedMutPtr<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { self.ptr.as_mut().unwrap_unchecked() }
    }
}
