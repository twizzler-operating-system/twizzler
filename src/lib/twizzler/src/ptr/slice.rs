use std::{
    borrow::Cow,
    ops::{Deref, DerefMut, Index, IndexMut},
};

use twizzler_runtime_api::{FotResolveError, ObjID, ObjectHandle};

use super::{InvPtr, InvPtrBuilder, OnceHandle, ResolvedPtr};
use crate::{
    marker::{Invariant, StoreEffect},
    object::fot::FotEntry,
};

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

    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    /// Get the invariant pointer to the start of the slice.
    pub fn ptr(&self) -> &InvPtr<T> {
        &self.ptr
    }

    /// Get the length of the slice.
    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_local(&self) -> bool {
        todo!()
    }

    pub fn resolve(&self) -> ResolvedSlice<'_, T> {
        self.try_resolve().unwrap()
    }

    /// Resolves an invariant slice.
    ///
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub fn try_resolve(&self) -> Result<ResolvedSlice<'_, T>, FotResolveError> {
        let resolved = self.ptr.try_resolve()?;
        println!("resolved slice: {:p}", resolved.ptr());
        Ok(ResolvedSlice::new(resolved, self.len as usize))
    }
}

pub struct ResolvedSlice<'obj, T> {
    ptr: ResolvedPtr<'obj, T>,
    len: usize,
}

impl<'obj, T> ResolvedSlice<'obj, T> {
    pub unsafe fn as_mut(&self) -> ResolvedMutSlice<'obj, T> {
        todo!()
    }

    fn new(ptr: ResolvedPtr<'obj, T>, len: usize) -> Self {
        Self { len, ptr }
    }

    pub fn ptr(&self) -> &ResolvedPtr<'obj, T> {
        &self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn owned<'a>(&self) -> ResolvedSlice<'a, T> {
        ResolvedSlice {
            ptr: self.ptr().owned(),
            len: self.len(),
        }
    }

    pub fn get(&self, idx: usize) -> Option<ResolvedPtr<'obj, T>> {
        if idx >= self.len() {
            None
        } else {
            Some(unsafe { self.ptr().clone().add(idx) })
        }
    }
}

impl<'obj, T> Deref for ResolvedSlice<'obj, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        // Safety: ResolvedSlice ensures this is correct.
        unsafe { std::slice::from_raw_parts(self.ptr.ptr(), self.len) }
    }
}

/*
impl<'obj, T, Idx: Into<usize>> IndexMut<Idx> for ResolvedMutSlice<'obj, T> {
    fn index_mut(&mut self, index: Idx) -> &mut Self::Output {
        let slice = unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) };
        &mut slice[index.into()]
    }
}

impl<'obj, T, Idx: Into<usize>> Index<Idx> for ResolvedMutSlice<'obj, T> {
    type Output = T;

    fn index(&self, index: Idx) -> &Self::Output {
        let slice = unsafe { core::slice::from_raw_parts(self.ptr, self.len) };
        &slice[index.into()]
    }
}

impl<'obj, T, Idx: Into<usize>> Index<Idx> for ResolvedSlice<'obj, T> {
    type Output = T;

    fn index(&self, index: Idx) -> &Self::Output {
        let slice = unsafe { core::slice::from_raw_parts(self.ptr.ptr(), self.len) };
        &slice[index.into()]
    }
}
*/

pub struct ResolvedMutSlice<'obj, T> {
    handle: &'obj ObjectHandle,
    len: usize,
    ptr: *mut T,
}

impl<'obj, T> ResolvedMutSlice<'obj, T> {
    pub fn handle(&self) -> &ObjectHandle {
        self.handle
    }

    pub fn ptr(&self) -> *mut T {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl<'obj, T> Deref for ResolvedMutSlice<'obj, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        // Safety: ResolvedSlice ensures this is correct.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl<'obj, T> DerefMut for ResolvedMutSlice<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: ResolvedSlice ensures this is correct.
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

pub struct InvSliceBuilder<T> {
    ptr: InvPtrBuilder<T>,
    len: usize,
}

impl<T> InvSliceBuilder<T> {
    pub const fn is_local(&self) -> bool {
        self.ptr.is_local()
    }

    pub const fn null() -> Self {
        Self {
            ptr: InvPtrBuilder::null(),
            len: 0,
        }
    }

    pub const fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    pub const fn id(&self) -> ObjID {
        self.ptr.id()
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn offset(&self) -> u64 {
        self.ptr.offset()
    }

    pub fn fot_entry(&self) -> FotEntry {
        self.ptr.fot_entry()
    }

    pub const fn ptr(&self) -> &InvPtrBuilder<T> {
        &self.ptr
    }

    pub const fn into_raw_parts(self) -> (InvPtrBuilder<T>, usize) {
        let Self { ptr, len } = self;
        (ptr, len)
    }

    pub const unsafe fn from_raw_parts(ptr: InvPtrBuilder<T>, len: usize) -> Self {
        Self { ptr, len }
    }
}

impl<T> StoreEffect for InvSlice<T> {
    type MoveCtor = InvSliceBuilder<T>;

    fn store<'a>(ctor: Self::MoveCtor, in_place: &mut crate::marker::InPlace<'a>) -> Self
    where
        Self: Sized,
    {
        Self {
            len: ctor.len() as u64,
            ptr: in_place.store(ctor.ptr),
        }
    }
}
