use super::TxHandle;

pub struct UnsafeTxHandle {}

impl UnsafeTxHandle {
    pub unsafe fn new() -> Self {
        UnsafeTxHandle {}
    }
}

impl TxHandle for UnsafeTxHandle {
    fn tx_mut(&self, data: *const u8, len: usize) -> super::Result<*mut u8> {
        todo!()
    }
}
