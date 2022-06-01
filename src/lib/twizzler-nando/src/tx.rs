use std::sync::Arc;

use crate::{
    cell::TxCell,
    ptr::{EffAddr, InvPtr, LeaError},
    Object, slot::Slot,
};

pub trait TxHandle {
    fn txcell_get<'a, T>(&self, cell: &'a TxCell<T>) -> Result<&'a T, TxError>;
    fn txcell_get_mut<'a, T>(&self, cell: &'a TxCell<T>) -> Result<&'a mut T, TxError>;

    fn base<'a, T>(&self, obj: &'a Object<T>) -> &'a T;

    fn ptr_resolve<Target>(
        &self,
        ptr: &InvPtr<Target>,
        obj: &Arc<Slot>,
    ) -> Result<EffAddr<Target>, LeaError>;
}

#[repr(C)]
pub enum TxError {
    Unknown,
    TooBig,
}
