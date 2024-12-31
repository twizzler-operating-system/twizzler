use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut, Index, IndexMut},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::GlobalPtr;

pub struct Ref<'obj, T> {
    ptr: *const T,
    handle: *const ObjectHandle,
    _pd: PhantomData<&'obj T>,
}

impl<'obj, T> Ref<'obj, T> {
    pub fn raw(&self) -> *const T {
        self.ptr
    }

    pub unsafe fn cast<U>(self) -> Ref<'obj, U> {
        todo!()
    }

    pub unsafe fn mutable(self) -> RefMut<'obj, T> {
        todo!()
    }
}

impl<'obj, T> Deref for Ref<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'a, T> From<Ref<'a, T>> for GlobalPtr<T> {
    fn from(value: Ref<'a, T>) -> Self {
        todo!()
    }
}

pub struct RefMut<'obj, T> {
    ptr: *mut T,
    handle: *const ObjectHandle,
    _pd: PhantomData<&'obj mut T>,
}

impl<'obj, T> RefMut<'obj, T> {
    pub fn raw(&self) -> *mut T {
        self.ptr
    }

    pub unsafe fn cast<U>(self) -> RefMut<'obj, U> {
        todo!()
    }
}

impl<'obj, T> Deref for RefMut<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'obj, T> DerefMut for RefMut<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut().unwrap_unchecked() }
    }
}

impl<'a, T> From<RefMut<'a, T>> for GlobalPtr<T> {
    fn from(value: RefMut<'a, T>) -> Self {
        todo!()
    }
}

pub struct RefSlice<'a, T> {
    ptr: Ref<'a, T>,
    len: usize,
}

impl<'a, T> RefSlice<'a, T> {
    pub unsafe fn from_ref(ptr: Ref<'a, T>, len: usize) -> Self {
        Self { ptr, len }
    }

    pub fn as_slice(&self) -> &[T] {
        let raw_ptr = self.ptr.raw();
        unsafe { core::slice::from_raw_parts(raw_ptr, self.len) }
    }

    pub fn get(&self, idx: usize) -> Option<Ref<'a, T>> {
        todo!()
    }
}

impl<'a, T> Index<usize> for RefSlice<'a, T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        let slice = self.as_slice();
        &slice[index]
    }
}

pub struct RefSliceMut<'a, T> {
    ptr: RefMut<'a, T>,
    len: usize,
}

impl<'a, T> RefSliceMut<'a, T> {
    pub unsafe fn from_ref(ptr: RefMut<'a, T>, len: usize) -> Self {
        Self { ptr, len }
    }

    pub fn as_slice(&self) -> &[T] {
        let raw_ptr = self.ptr.raw();
        unsafe { core::slice::from_raw_parts(raw_ptr, self.len) }
    }

    pub fn as_slice_mut(&mut self) -> &mut [T] {
        let raw_ptr = self.ptr.raw();
        unsafe { core::slice::from_raw_parts_mut(raw_ptr, self.len) }
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<RefMut<'_, T>> {
        todo!()
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
