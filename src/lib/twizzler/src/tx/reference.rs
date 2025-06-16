use std::{
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::TxObject;
use crate::{object::RawObject, ptr::RefMut};

pub struct TxRef<T> {
    ptr: *mut T,
    tx: Option<TxObject<()>>,
}

impl<T> TxRef<T> {
    pub fn as_mut(&mut self) -> RefMut<'_, T> {
        let handle = self.tx.as_ref().unwrap().handle().handle();
        unsafe { RefMut::from_raw_parts(self.ptr, handle) }
    }

    pub unsafe fn from_raw_parts<B>(tx: TxObject<B>, ptr: *mut T) -> Self {
        Self {
            ptr,
            tx: Some(tx.into_unit()),
        }
    }

    pub fn tx(&self) -> &TxObject<()> {
        self.tx.as_ref().unwrap()
    }

    pub fn tx_mut(&mut self) -> &mut TxObject<()> {
        self.tx.as_mut().unwrap()
    }

    pub fn into_tx(mut self) -> TxObject<()> {
        self.tx.take().unwrap()
    }

    pub fn raw(&self) -> *mut T {
        self.ptr
    }
}

impl<T> TxRef<MaybeUninit<T>> {
    pub fn write(mut self, val: T) -> crate::tx::Result<TxRef<T>> {
        unsafe {
            let ptr = self.ptr.as_mut().unwrap_unchecked();
            let tx = self.tx.take().unwrap();
            Ok(TxRef::<T>::from_raw_parts(tx, ptr.write(val)))
        }
    }
}

impl<T> Deref for TxRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<T> DerefMut for TxRef<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut().unwrap_unchecked() }
    }
}

impl<T> Drop for TxRef<T> {
    #[track_caller]
    fn drop(&mut self) {
        let _ = self.tx.take().map(|tx| tx.commit());
    }
}

impl<T> Into<ObjectHandle> for TxRef<T> {
    fn into(self) -> ObjectHandle {
        self.tx().handle().clone()
    }
}

impl<T> Into<ObjectHandle> for &TxRef<T> {
    fn into(self) -> ObjectHandle {
        self.tx().handle().clone()
    }
}

impl<T> AsRef<ObjectHandle> for TxRef<T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.tx().handle()
    }
}
