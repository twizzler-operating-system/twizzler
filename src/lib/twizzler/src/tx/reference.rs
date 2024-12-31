use std::{cell::RefMut, mem::MaybeUninit, ops::Deref};

use super::TxObject;

pub struct TxRef<T> {
    ptr: *const T,
    tx: TxObject<()>,
    panic_on_drop: bool,
}

impl<T> TxRef<T> {
    pub fn as_mut(&mut self) -> RefMut<'_, T> {
        todo!()
    }

    pub fn tx(&self) -> &TxObject<()> {
        todo!()
    }
}

impl<T> TxRef<MaybeUninit<T>> {
    pub fn write(self, val: T) -> crate::tx::Result<TxRef<T>> {
        todo!()
    }
}

impl<T> Deref for TxRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<T> Drop for TxRef<T> {
    fn drop(&mut self) {
        todo!()
    }
}
