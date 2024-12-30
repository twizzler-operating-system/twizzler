use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::GlobalPtr;

pub struct Ref<'obj, T> {
    ptr: *const T,
    handle: *const ObjectHandle,
    _pd: PhantomData<&'obj T>,
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
