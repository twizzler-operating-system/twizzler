use std::{
    borrow::Cow,
    marker::{PhantomData, PhantomPinned},
    ops::{Deref, DerefMut},
};

use twizzler_runtime_api::ObjectHandle;

use super::InvPtrBuilder;

// TODO: niche optimization -- sizeof Option<InvPtr<T>> == 8 -- null => None.
#[repr(transparent)]
pub struct InvPtr<T> {
    bits: u64,
    _pd: PhantomData<*const T>,
    _pp: PhantomPinned,
}

// Safety: These are the standard library rules for references (https://doc.rust-lang.org/std/primitive.reference.html).
unsafe impl<T: Sync> Sync for InvPtr<T> {}
unsafe impl<T: Sync> Send for InvPtr<T> {}

impl<T> InvPtr<T> {
    pub fn null() -> Self {
        Self {
            bits: 0,
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    pub fn set(&mut self, builder: impl Into<InvPtrBuilder<T>>) {
        todo!()
    }

    pub fn raw(&self) -> u64 {
        self.bits
    }

    /// Resolves an invariant pointer.
    ///
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub fn resolve(&self) -> Result<ResolvedPtr<'_, T>, ()> {
        todo!()
    }
}

pub struct ResolvedPtr<'obj, T> {
    handle: Cow<'obj, ObjectHandle>,
    ptr: *const T,
}

impl<'obj, T> ResolvedPtr<'obj, T> {
    pub unsafe fn as_mut(&self) -> ResolvedMutablePtr<'obj, T> {
        todo!()
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
