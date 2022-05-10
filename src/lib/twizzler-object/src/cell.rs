use std::cell::UnsafeCell;

use twizzler_abi::marker::ObjSafe;

use crate::tx::TxHandle;

pub struct TxCell<T> {
    item: UnsafeCell<T>,
}

impl<T: ObjSafe> ObjSafe for TxCell<T> {}

impl<T: ObjSafe> TxCell<T> {
    pub fn get_mut(&self, _tx: &TxHandle) -> &mut T {
        unsafe { self.item.get().as_mut().unwrap() }
    }
}
