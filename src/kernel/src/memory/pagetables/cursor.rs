use crate::arch::address::VirtAddr;

#[derive(Debug, Clone, Copy)]
/// A type that refers to a region within the virtual address space.
pub struct MappingCursor {
    start: VirtAddr,
    len: usize,
}

impl MappingCursor {
    /// Construct a new mapping cursor.
    pub fn new(start: VirtAddr, len: usize) -> Self {
        Self { start, len }
    }

    /// Advance the cursor by `len`. Should the resulting address be non-canonical, `None` is returned.
    pub fn advance(mut self, len: usize) -> Option<Self> {
        if self.len <= len {
            return None;
        }
        let vaddr = self.start.offset(len as isize).ok()?;
        self.start = vaddr;
        self.len -= len;
        Some(self)
    }

    /// How many bytes remain?
    pub fn remaining(&self) -> usize {
        self.len
    }

    /// Get the start of the region.
    pub fn start(&self) -> VirtAddr {
        self.start
    }
}
