use std::marker::PhantomData;

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
    pub fn new_in<'a>(target: &impl TxHandle<'a>) -> Storable<Self> {
        todo!()
    }
}
