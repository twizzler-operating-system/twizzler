use std::{
    borrow::{Borrow, BorrowMut},
    ops::{Deref, DerefMut, Index, IndexMut, RangeBounds},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::{Ref, RefMut, RefSlice};
use crate::{ptr::GlobalPtr, util::range_bounds_to_start_and_end};

pub struct RefSliceMut<'a, T> {
    ptr: RefMut<'a, T>,
    len: usize,
}

impl<'a, T> RefSliceMut<'a, T> {
    pub unsafe fn from_ref(ptr: RefMut<'a, T>, len: usize) -> Self {
        Self { ptr, len }
    }

    pub fn offset(&self) -> u64 {
        self.ptr.offset()
    }

    pub fn as_slice(&self) -> &'a [T] {
        unsafe { core::slice::from_raw_parts(self.ptr.raw(), self.len) }
    }

    pub fn as_slice_mut(&mut self) -> &'a mut [T] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.raw(), self.len) }
    }

    #[inline]
    pub fn get_ref(&self, idx: usize) -> Option<Ref<'a, T>> {
        let ptr = self.as_slice().get(idx)?;
        Some(unsafe { Ref::from_ptr(ptr) })
    }

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&T> {
        let ptr = self.as_slice().get(idx)?;
        Some(ptr)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<RefMut<'a, T>> {
        let ptr = self.as_slice_mut().get_mut(idx)?;
        Some(unsafe { RefMut::from_ptr(ptr) })
    }

    #[inline]
    pub fn get_into(mut self, idx: usize) -> Option<RefMut<'a, T>> {
        let ptr = self.as_slice_mut().get_mut(idx)? as *mut T;
        Some(RefMut::new(ptr, self.ptr.lazy_handle))
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.ptr.handle()
    }

    #[inline]
    pub fn slice(mut self, range: impl RangeBounds<usize>) -> Self {
        let (start, end) = range_bounds_to_start_and_end(self.len, range);
        let len = end - start;
        if let Some(r) = self.get_mut(start) {
            unsafe { Self::from_ref(r, len) }
        } else {
            unsafe { Self::from_ref(self.ptr, 0) }
        }
    }

    pub fn into_ref_slice(self) -> RefSlice<'a, T> {
        let len = self.len();
        unsafe { RefSlice::from_ref(self.ptr.into_ref(), len) }
    }

    pub fn as_ref_slice(&self) -> RefSlice<'a, T> {
        unsafe { RefSlice::from_ref(self.ptr.as_ref().owned(), self.len()) }
    }
}

impl<'a, T> From<RefSliceMut<'a, T>> for GlobalPtr<T> {
    fn from(value: RefSliceMut<'a, T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

impl<'a, T> Index<usize> for RefSliceMut<'a, T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        let slice = self.as_slice();
        &slice[index]
    }
}

impl<'a, T> IndexMut<usize> for RefSliceMut<'a, T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let slice = self.as_slice_mut();
        &mut slice[index]
    }
}

impl<'a, T> Deref for RefSliceMut<'a, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<'a, T> DerefMut for RefSliceMut<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_slice_mut()
    }
}

impl<'a, T> AsRef<[T]> for RefSliceMut<'a, T> {
    fn as_ref(&self) -> &[T] {
        &*self
    }
}

impl<'a, T> AsMut<[T]> for RefSliceMut<'a, T> {
    fn as_mut(&mut self) -> &mut [T] {
        &mut *self
    }
}

impl<'a, T> Borrow<[T]> for RefSliceMut<'a, T> {
    fn borrow(&self) -> &[T] {
        &*self
    }
}

impl<'a, T> BorrowMut<[T]> for RefSliceMut<'a, T> {
    fn borrow_mut(&mut self) -> &mut [T] {
        &mut *self
    }
}

impl<'a, T> Into<ObjectHandle> for RefSliceMut<'a, T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<'a, T> Into<ObjectHandle> for &RefSliceMut<'a, T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<'a, T> AsRef<ObjectHandle> for RefSliceMut<'a, T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.handle()
    }
}
