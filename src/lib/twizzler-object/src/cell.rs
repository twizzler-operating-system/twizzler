use std::cell::UnsafeCell;

use twizzler_abi::marker::ObjSafe;

use crate::tx::{TxError, TxHandle};

#[repr(transparent)]
pub struct TxCell<T> {
    item: UnsafeCell<T>,
}

unsafe impl<T: ObjSafe> ObjSafe for TxCell<T> {}

impl<T: ObjSafe> TxCell<T> {
    pub fn get_mut(&self, tx: impl TxHandle) -> Result<&mut T, TxError> {
        tx.txcell_get_mut(&self)?;
        Ok(unsafe { self.item.get().as_mut().unwrap_unchecked() })
    }

    pub fn get(&self, tx: impl TxHandle) -> Result<&T, TxError> {
        tx.txcell_get(&self)?;
        Ok(unsafe { self.item.get().as_ref().unwrap_unchecked() })
    }

    pub unsafe fn get_unchecked(&self) -> &T {
        self.item.get().as_ref().unwrap_unchecked()
    }

    pub unsafe fn get_mut_unchecked(&self) -> &mut T {
        self.item.get().as_mut().unwrap_unchecked()
    }
}
