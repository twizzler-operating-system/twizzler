use std::{
    borrow::Cow,
    marker::{PhantomData, PhantomPinned},
    ops::{Deref, DerefMut},
};

use twizzler_runtime_api::{ObjID, ObjectHandle};

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
    pub fn set(&mut self, builder: InvPtrBuilder<T>) {
        todo!()
    }

    pub fn raw(&self) -> u64 {
        self.bits
    }

    /// Resolves an invariant pointer.
    ///
    /// The resulting pointer does NOT implement Deref, since that is not safe in-general.
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub fn resolve(&self) -> Result<ResolvedPtr<'_, T>, ()> {
        todo!()
    }

    /// Resolves an invariant pointer.
    ///
    /// The resulting pointer implements Deref and DerefMut.
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub fn resolve_mut(&self) -> Result<ResolvedMutablePtr<'_, T>, ()> {
        todo!()
    }

    /// Resolves an invariant pointer.
    ///
    /// The resulting pointer implements Deref.
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub fn resolve_imm(&self) -> Result<ResolvedImmutablePtr<'_, T>, ()> {
        todo!()
    }
}

pub struct InvPtrBuilder<T> {
    id: ObjID,
    offset: u64,
    _pd: PhantomData<*const T>,
}

impl<T> InvPtrBuilder<T> {
    pub unsafe fn new_id(id: ObjID, offset: u64) -> Self {
        Self {
            id,
            offset,
            _pd: PhantomData,
        }
    }
}

pub struct ResolvedPtr<'obj, T> {
    handle: Cow<'obj, ObjectHandle>,
    ptr: *const T,
}

impl<'obj, T> ResolvedPtr<'obj, T> {
    pub fn write(&self, data: T) {
        todo!()
    }

    pub unsafe fn as_ref(&self) -> &T {
        todo!()
    }
}

impl<'obj, T: Copy> ResolvedPtr<'obj, T> {
    pub fn read(&self) -> T {
        todo!()
    }
}

pub struct ResolvedImmutablePtr<'obj, T> {
    handle: Cow<'obj, ObjectHandle>,
    ptr: *const T,
}

impl<'obj, T> Deref for ResolvedImmutablePtr<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Safety: we are pointing to an immutable object.
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'obj, T: Copy> ResolvedImmutablePtr<'obj, T> {
    pub fn read(&self) -> T {
        todo!()
    }
}

pub struct ResolvedMutablePtr<'obj, T> {
    handle: &'obj ObjectHandle,
    ptr: *mut T,
}

impl<'obj, T> ResolvedMutablePtr<'obj, T> {
    pub fn write(&self, data: T) {
        todo!()
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

impl<'obj, T: Copy> ResolvedMutablePtr<'obj, T> {
    pub fn read(&self) -> T {
        todo!()
    }
}
