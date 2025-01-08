use std::marker::PhantomData;

use super::{GlobalPtr, Ref};
use crate::{
    marker::{Invariant, PhantomStoreEffect},
    tx::{Result, TxHandle, TxObject},
};

#[repr(C)]
#[derive(Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct InvPtr<T: Invariant> {
    value: u64,
    _pse: PhantomStoreEffect,
    _pd: PhantomData<*const T>,
}

impl<T: Invariant> InvPtr<T> {
    pub fn global(&self) -> GlobalPtr<T> {
        todo!()
    }

    pub unsafe fn resolve(&self) -> Ref<'_, T> {
        todo!()
    }

    pub fn set(&mut self, ptr: impl Into<GlobalPtr<u8>>, tx: &impl TxHandle) -> Result<()> {
        todo!()
    }

    pub fn null() -> Self {
        Self::from_raw_parts(0, 0)
    }

    pub fn from_raw_parts(idx: u64, offset: u64) -> Self {
        Self {
            value: (idx << 48) | offset,
            _pse: PhantomStoreEffect,
            _pd: PhantomData,
        }
    }

    pub fn raw(&self) -> u64 {
        self.value
    }

    pub fn new<B>(tx: &TxObject<B>, gp: impl Into<GlobalPtr<T>>) -> crate::tx::Result<Self> {
        todo!()
    }
}
