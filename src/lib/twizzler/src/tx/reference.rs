use std::{mem::MaybeUninit, ops::Deref};

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

    pub unsafe fn new<B>(tx: TxObject<B>, ptr: *mut T) -> Self {
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
}

impl<T> TxRef<MaybeUninit<T>> {
    pub fn write(mut self, val: T) -> crate::tx::Result<TxRef<T>> {
        unsafe {
            let ptr = self.ptr.as_mut().unwrap_unchecked();
            let tx = self.tx.take().unwrap();
            Ok(TxRef::<T>::new(tx, ptr.write(val)))
        }
    }
}

impl<T> Deref for TxRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<T> Drop for TxRef<T> {
    #[track_caller]
    fn drop(&mut self) {
        todo!()
    }
}
