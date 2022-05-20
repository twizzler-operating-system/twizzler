use crate::cell::TxCell;

pub trait TxHandle {
    fn txcell_get<T>(&self, cell: &TxCell<T>) -> Result<(), TxError>;
    fn txcell_get_mut<T>(&self, cell: &TxCell<T>) -> Result<(), TxError>;
}

#[repr(C)]
pub enum TxError {
    Unknown,
    TooBig,
}
