use std::{
    borrow::Cow,
    ops::{Deref, DerefMut, Index, IndexMut},
};

use twizzler_runtime_api::{FotResolveError, ObjID, ObjectHandle};

use super::{InvPtr, InvPtrBuilder, OnceHandle, ResolvedMutPtr, ResolvedPtr};
use crate::{
    marker::{Invariant, StoreEffect},
    object::fot::FotEntry,
};

#[derive(twizzler_derive::Invariant)]
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

    pub unsafe fn resolve(&self) -> ResolvedSlice<'_, T> {
        self.try_resolve().unwrap()
    }

    /// Resolves an invariant slice.
    ///
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub unsafe fn try_resolve(&self) -> Result<ResolvedSlice<'_, T>, FotResolveError> {
        let resolved = self.ptr.try_resolve()?;
        Ok(ResolvedSlice::from_raw_parts(resolved, self.len as usize))
    }
}

pub struct ResolvedSlice<'obj, T> {
    ptr: ResolvedPtr<'obj, T>,
    len: usize,
}

impl<'obj, T> ResolvedSlice<'obj, T> {
    pub unsafe fn into_mut(self) -> ResolvedMutSlice<'obj, T> {
        let ResolvedSlice { ptr, len } = self;

        ResolvedMutSlice {
            ptr: ptr.into_mut(),
            len,
        }
    }

    pub unsafe fn from_raw_parts(ptr: ResolvedPtr<'obj, T>, len: usize) -> Self {
        Self { len, ptr }
    }

    pub fn ptr(&self) -> &ResolvedPtr<'obj, T> {
        &self.ptr
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn owned<'a>(self) -> ResolvedSlice<'a, T> {
        let ResolvedSlice { ptr, len } = self;
        ResolvedSlice {
            ptr: ptr.owned(),
            len,
        }
    }

    pub fn get(&self, idx: usize) -> Option<ResolvedPtr<'obj, T>> {
        if idx >= self.len() {
            None
        } else {
            let ptr = unsafe { ResolvedPtr::new(self.ptr.ptr().add(idx)) };
            Some(ptr)
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

impl<'obj, T, Idx: Into<usize>> Index<Idx> for ResolvedSlice<'obj, T> {
    type Output = T;

    fn index(&self, index: Idx) -> &Self::Output {
        let slice = unsafe { core::slice::from_raw_parts(self.ptr.ptr(), self.len) };
        &slice[index.into()]
    }
}

pub struct ResolvedMutSlice<'obj, T> {
    ptr: ResolvedMutPtr<'obj, T>,
    len: usize,
}

impl<'obj, T> ResolvedMutSlice<'obj, T> {
    pub unsafe fn from_raw_parts(ptr: ResolvedMutPtr<'obj, T>, len: usize) -> Self {
        Self { len, ptr }
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.ptr.handle()
    }

    pub fn ptr(&self) -> *mut T {
        self.ptr.ptr()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn owned<'a>(self) -> ResolvedMutSlice<'a, T> {
        let ResolvedMutSlice { ptr, len } = self;
        ResolvedMutSlice {
            ptr: ptr.owned(),
            len,
        }
    }

    pub fn get(&self, idx: usize) -> Option<ResolvedMutPtr<'obj, T>> {
        if idx >= self.len() {
            None
        } else {
            let ptr = unsafe { ResolvedMutPtr::new(self.ptr.ptr().add(idx)) };
            Some(ptr)
        }
    }
}

impl<'obj, T> Deref for ResolvedMutSlice<'obj, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        // Safety: ResolvedSlice ensures this is correct.
        unsafe { std::slice::from_raw_parts(self.ptr.ptr(), self.len) }
    }
}

impl<'obj, T: Unpin> DerefMut for ResolvedMutSlice<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: ResolvedMutSlice ensures this is correct.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.ptr(), self.len) }
    }
}

impl<'a, T> From<ResolvedMutSlice<'a, T>> for ResolvedSlice<'a, T> {
    fn from(value: ResolvedMutSlice<'a, T>) -> Self {
        Self {
            ptr: value.ptr.into(),
            len: value.len,
        }
    }
}

impl<'obj, T: Unpin, Idx: Into<usize>> IndexMut<Idx> for ResolvedMutSlice<'obj, T> {
    fn index_mut(&mut self, index: Idx) -> &mut Self::Output {
        let slice = unsafe { core::slice::from_raw_parts_mut(self.ptr.ptr(), self.len) };
        &mut slice[index.into()]
    }
}

impl<'obj, T, Idx: Into<usize>> Index<Idx> for ResolvedMutSlice<'obj, T> {
    type Output = T;

    fn index(&self, index: Idx) -> &Self::Output {
        let slice = unsafe { core::slice::from_raw_parts(self.ptr.ptr(), self.len) };
        &slice[index.into()]
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

    fn store<'a>(ctor: Self::MoveCtor, in_place: &mut crate::marker::StorePlace<'a>) -> Self
    where
        Self: Sized,
    {
        Self {
            len: ctor.len() as u64,
            ptr: in_place.store(ctor.ptr),
        }
    }
}
