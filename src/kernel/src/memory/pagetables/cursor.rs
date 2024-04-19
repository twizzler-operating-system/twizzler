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

    /// Advance the cursor by `len`. Should the resulting address be non-canonical, `None` is
    /// returned.
    pub fn advance(mut self, len: usize) -> Option<Self> {
        if self.len <= len {
            return None;
        }
        let vaddr = self.start.offset(len).ok()?;
        self.start = vaddr;
        self.len -= len;
        Some(self)
    }

    /// Advance the cursor by up to `len`, so we end up aligned on len. Should the resulting address
    /// be non-canonical, `None` is returned.
    pub fn align_advance(mut self, len: usize) -> Option<Self> {
        let vaddr = self.start.align_up(len as u64).ok()?;
        if vaddr == self.start {
            if self.len <= len {
                return None;
            }
            self.start = self.start.offset(len).ok()?;
            self.len -= len;
        } else {
            let thislen = vaddr - self.start;
            if self.len <= thislen {
                return None;
            }
            self.len -= thislen;
            self.start = vaddr;
        }
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
