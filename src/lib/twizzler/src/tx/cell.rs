use std::cell::UnsafeCell;

use super::{TxObject, TxRef};
use crate::object::Object;

#[repr(transparent)]
pub struct TxCell<T>(UnsafeCell<T>);

impl<T: Clone> Clone for TxCell<T> {
    fn clone(&self) -> Self {
        Self(UnsafeCell::new(unsafe {
            self.0.get().as_ref().unwrap().clone()
        }))
    }
}

impl<T> TxCell<T> {
    pub fn new(inner: T) -> Self {
        Self(UnsafeCell::new(inner))
    }

    pub unsafe fn as_mut(&self) -> &mut T {
        unsafe { self.0.get().as_mut().unwrap_unchecked() }
    }

    pub fn get(&self) -> crate::Result<TxRef<T>> {
        let inner = self.0.get();
        let handle = twizzler_rt_abi::object::twz_rt_get_object_handle(inner.cast())?;
        let tx = TxObject::new(unsafe { Object::<()>::from_handle_unchecked(handle) })?;
        Ok(unsafe { TxRef::from_raw_parts(tx, inner) })
    }
}

impl<T> std::ops::Deref for TxCell<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.get().as_ref().unwrap() }
    }
}

impl<T> std::ops::DerefMut for TxCell<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.get().as_mut().unwrap() }
    }
}
