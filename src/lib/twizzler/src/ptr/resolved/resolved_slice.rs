use std::{
    borrow::Borrow,
    ops::{Deref, Index, RangeBounds},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::{Ref, RefSliceMut};
use crate::{
    ptr::{GlobalPtr, TxRefSlice},
    util::range_bounds_to_start_and_end,
};

pub struct RefSlice<'a, T> {
    ptr: Ref<'a, T>,
    len: usize,
}

impl<'a, T> RefSlice<'a, T> {
    #[inline]
    pub unsafe fn from_ref(ptr: Ref<'a, T>, len: usize) -> Self {
        Self { ptr, len }
    }

    pub fn offset(&self) -> u64 {
        self.ptr.offset()
    }

    #[inline]
    pub fn as_slice(&self) -> &'a [T] {
        unsafe { core::slice::from_raw_parts(self.ptr.raw(), self.len) }
    }

    #[inline]
    pub fn slice(self, range: impl RangeBounds<usize>) -> Self {
        let (start, end) = range_bounds_to_start_and_end(self.len, range);
        let len = end - start;
        if let Some(r) = self.get_ref(start) {
            unsafe { Self::from_ref(r, len) }
        } else {
            unsafe { Self::from_ref(self.ptr, 0) }
        }
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

    #[inline]
    pub fn get_into(self, idx: usize) -> Option<Ref<'a, T>> {
        let ptr = self.as_slice().get(idx)? as *const T;
        Some(Ref::new(ptr, self.ptr.lazy_handle))
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.ptr.handle()
    }

    pub fn into_tx(self) -> crate::Result<TxRefSlice<T>> {
        self.as_tx()
    }

    pub fn as_tx(&self) -> crate::Result<TxRefSlice<T>> {
        let len = self.len();
        Ok(unsafe { TxRefSlice::from_ref(self.ptr.as_tx()?, len) })
    }

    pub unsafe fn into_mut(self) -> crate::Result<RefSliceMut<'a, T>> {
        self.as_mut()
    }

    pub unsafe fn as_mut(&self) -> crate::Result<RefSliceMut<'a, T>> {
        let len = self.len();
        Ok(unsafe { RefSliceMut::from_ref(self.ptr.as_mut(), len) })
    }
}

impl<'a, T> From<RefSlice<'a, T>> for GlobalPtr<T> {
    fn from(value: RefSlice<'a, T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

impl<'a, T> Index<usize> for RefSlice<'a, T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        let slice = self.as_slice();
        &slice[index]
    }
}

impl<'a, T> Deref for RefSlice<'a, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<'a, T> AsRef<[T]> for RefSlice<'a, T> {
    fn as_ref(&self) -> &[T] {
        &*self
    }
}

impl<'a, T> Borrow<[T]> for RefSlice<'a, T> {
    fn borrow(&self) -> &[T] {
        &*self
    }
}

impl<'a, T> Into<ObjectHandle> for RefSlice<'a, T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<'a, T> Into<ObjectHandle> for &RefSlice<'a, T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<'a, T> AsRef<ObjectHandle> for RefSlice<'a, T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.handle()
    }
}
