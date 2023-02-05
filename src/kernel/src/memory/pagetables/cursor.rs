use crate::arch::address::VirtAddr;

#[derive(Debug, Clone, Copy)]
pub struct MappingCursor {
    start: VirtAddr,
    len: usize,
}

impl MappingCursor {
    pub fn new(start: VirtAddr, len: usize) -> Self {
        Self { start, len }
    }

    pub fn advance(mut self, len: usize) -> Option<Self> {
        if self.len <= len {
            return None;
        }
        let vaddr = self.start.offset(len as isize).ok()?;
        self.start = vaddr;
        self.len -= len;
        Some(self)
    }

    pub fn remaining(&self) -> usize {
        self.len
    }

    pub fn start(&self) -> VirtAddr {
        self.start
    }
}
