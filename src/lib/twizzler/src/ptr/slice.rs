use std::{
    borrow::Cow,
    ops::{Deref, DerefMut},
};

use twizzler_runtime_api::ObjectHandle;

use super::{InvPtr, InvPtrBuilder};

#[repr(C)]
pub struct InvSlice<T> {
    len: u64,
    ptr: InvPtr<T>,
}

// Safety: These are the standard library rules for references (https://doc.rust-lang.org/std/primitive.reference.html).
unsafe impl<T: Sync> Sync for InvSlice<T> {}
unsafe impl<T: Sync> Send for InvSlice<T> {}

impl<T> InvSlice<T> {
    pub fn null() -> Self {
        Self {
            len: 0,
            ptr: InvPtr::null(),
        }
    }

    /// Construct a slice from raw parts.
    ///
    /// # Safety
    /// The caller must ensure that the pointed-to array is valid for [start, start + len), where
    /// start is the location pointed to by the invariant pointer.
    pub unsafe fn set_from_raw_parts(&mut self, builder: impl Into<InvPtrBuilder<T>>, len: usize) {
        todo!()
    }

    /// Get the invariant pointer to the start of the slice.
    pub fn ptr(&self) -> &InvPtr<T> {
        &self.ptr
    }

    /// Get the length of the slice.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Resolves an invariant slice.
    ///
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub fn resolve(&self) -> Result<ResolvedSlice<'_, T>, ()> {
        todo!()
    }
}

pub struct ResolvedSlice<'obj, T> {
    handle: Cow<'obj, ObjectHandle>,
    len: usize,
    ptr: *const T,
}

impl<'obj, T> ResolvedSlice<'obj, T> {
    pub unsafe fn as_mut(&self) -> ResolvedMutableSlice<'obj, T> {
        todo!()
    }
}

impl<'obj, T> Deref for ResolvedSlice<'obj, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        // Safety: ResolvedSlice ensures this is correct.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

pub struct ResolvedMutableSlice<'obj, T> {
    handle: &'obj ObjectHandle,
    len: usize,
    ptr: *mut T,
}

impl<'obj, T> Deref for ResolvedMutableSlice<'obj, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        // Safety: ResolvedSlice ensures this is correct.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl<'obj, T> DerefMut for ResolvedMutableSlice<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: ResolvedSlice ensures this is correct.
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}
