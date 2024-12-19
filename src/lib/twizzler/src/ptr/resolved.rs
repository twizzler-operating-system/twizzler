use std::{marker::PhantomData, ops::Deref};

use twizzler_rt_abi::object::ObjectHandle;

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
