use std::marker::PhantomData;

use super::{GlobalPtr, Ref};
use crate::{
    marker::{Invariant, PhantomStoreEffect, Storable},
    tx::TxHandle,
};

#[repr(C)]
#[derive(Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct InvPtr<T: Invariant> {
    value: u64,
    _pse: PhantomStoreEffect,
    _pd: PhantomData<*const T>,
}

impl<T: Invariant> InvPtr<T> {
    pub fn new_in(target: &impl TxHandle, global: impl Into<GlobalPtr<T>>) -> Storable<Self> {
        todo!()
    }

    pub fn resolve<'a>(&self) -> Ref<'a, T> {
        todo!()
    }
}
