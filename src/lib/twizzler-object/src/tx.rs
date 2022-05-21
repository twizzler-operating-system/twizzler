use crate::cell::TxCell;

pub trait TxHandle {
    fn txcell_get<'a, T>(&self, cell: &'a TxCell<T>) -> Result<&'a T, TxError>;
    fn txcell_get_mut<'a, T>(&self, cell: &'a TxCell<T>) -> Result<&'a mut T, TxError>;
}

#[repr(C)]
pub enum TxError {
    Unknown,
    TooBig,
}
