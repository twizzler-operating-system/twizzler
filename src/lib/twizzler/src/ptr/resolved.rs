use std::{
    borrow::Cow,
    ops::{Deref, DerefMut},
};

use twizzler_runtime_api::ObjectHandle;

pub struct ResolvedPtr<'obj, T> {
    handle: Cow<'obj, ObjectHandle>,
    ptr: *const T,
}

impl<'obj, T> ResolvedPtr<'obj, T> {
    pub unsafe fn as_mut(&self) -> ResolvedMutablePtr<'obj, T> {
        todo!()
    }

    pub fn handle(&self) -> &ObjectHandle {
        &self.handle
    }

    pub fn ptr(&self) -> *const T {
        self.ptr
    }
}

impl<'obj, T> Deref for ResolvedPtr<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

pub struct ResolvedMutablePtr<'obj, T> {
    handle: &'obj ObjectHandle,
    ptr: *mut T,
}

impl<'obj, T> ResolvedMutablePtr<'obj, T> {
    pub fn handle(&self) -> &ObjectHandle {
        self.handle
    }

    pub fn ptr(&self) -> *mut T {
        self.ptr
    }
}

impl<'obj, T> Deref for ResolvedMutablePtr<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'obj, T> DerefMut for ResolvedMutablePtr<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { self.ptr.as_mut().unwrap_unchecked() }
    }
}
