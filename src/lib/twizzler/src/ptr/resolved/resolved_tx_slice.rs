use std::{
    borrow::{Borrow, BorrowMut},
    ops::{Deref, DerefMut, Index, IndexMut, RangeBounds},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::{Ref, RefMut, TxRef};
use crate::{ptr::GlobalPtr, util::range_bounds_to_start_and_end};

pub struct TxRefSlice<T> {
    ptr: TxRef<T>,
    len: usize,
}

impl<T> TxRefSlice<T> {
    pub unsafe fn from_ref(ptr: TxRef<T>, len: usize) -> Self {
        Self { ptr, len }
    }

    #[inline]
    pub fn offset(&self) -> u64 {
        self.ptr.offset()
    }

    pub fn as_slice(&self) -> &[T] {
        unsafe { core::slice::from_raw_parts(self.ptr.raw(), self.len) }
    }

    pub fn as_slice_mut(&mut self) -> &mut [T] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.raw(), self.len) }
    }

    #[inline]
    pub fn get_ref(&self, idx: usize) -> Option<Ref<'_, T>> {
        let ptr = self.as_slice().get(idx)?;
        Some(unsafe { Ref::from_ptr(ptr) })
    }

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&T> {
        let ptr = self.as_slice().get(idx)?;
        Some(ptr)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<RefMut<'_, T>> {
        let ptr = self.as_slice_mut().get_mut(idx)?;
        Some(unsafe { RefMut::from_ptr(ptr) })
    }

    #[inline]
    pub fn get_into(mut self, idx: usize) -> Option<TxRef<T>> {
        let ptr = self.as_slice_mut().get_mut(idx)? as *mut T;
        Some(unsafe { TxRef::from_raw_parts(self.ptr.into_tx(), ptr) })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.ptr.handle()
    }

    #[inline]
    pub fn slice(self, range: impl RangeBounds<usize>) -> Self {
        let (start, end) = range_bounds_to_start_and_end(self.len, range);
        let len = end - start;
        if let Some(_) = self.get(start) {
            unsafe { Self::from_ref(self.get_into(start).unwrap(), len) }
        } else {
            unsafe { Self::from_ref(self.ptr, 0) }
        }
    }
}

impl<T> From<TxRefSlice<T>> for GlobalPtr<T> {
    fn from(value: TxRefSlice<T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

impl<T> Index<usize> for TxRefSlice<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        let slice = self.as_slice();
        &slice[index]
    }
}

impl<T> IndexMut<usize> for TxRefSlice<T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let slice = self.as_slice_mut();
        &mut slice[index]
    }
}

impl<T> Into<ObjectHandle> for TxRefSlice<T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<T> Into<ObjectHandle> for &TxRefSlice<T> {
    fn into(self) -> ObjectHandle {
        self.handle().clone()
    }
}

impl<T> AsRef<ObjectHandle> for TxRefSlice<T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.handle()
    }
}

impl<T> Deref for TxRefSlice<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T> DerefMut for TxRefSlice<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_slice_mut()
    }
}

impl<T> AsRef<[T]> for TxRefSlice<T> {
    fn as_ref(&self) -> &[T] {
        &*self
    }
}

impl<T> AsMut<[T]> for TxRefSlice<T> {
    fn as_mut(&mut self) -> &mut [T] {
        &mut *self
    }
}

impl<T> Borrow<[T]> for TxRefSlice<T> {
    fn borrow(&self) -> &[T] {
        &*self
    }
}

impl<T> BorrowMut<[T]> for TxRefSlice<T> {
    fn borrow_mut(&mut self) -> &mut [T] {
        &mut *self
    }
}
