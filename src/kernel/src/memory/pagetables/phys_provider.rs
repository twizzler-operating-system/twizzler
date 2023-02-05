use crate::arch::address::PhysAddr;

#[derive(Debug, Clone, Copy)]
pub struct PhysFrame {
    addr: PhysAddr,
    len: usize,
}

impl PhysFrame {
    pub fn new(addr: PhysAddr, len: usize) -> Self {
        Self { addr, len }
    }

    pub fn addr(&self) -> PhysAddr {
        self.addr
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

pub trait PhysAddrProvider {
    fn peek(&mut self) -> PhysFrame;
    fn consume(&mut self, len: usize);
}
