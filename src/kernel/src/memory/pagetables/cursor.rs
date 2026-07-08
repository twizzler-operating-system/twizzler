use crate::{arch::address::VirtAddr, memory::pagetables::Table};

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

    pub fn advance_until_empty(mut self, len: usize) -> Self {
        let remaining = self.len.min(len);
        self.start = self.start.offset(remaining).unwrap_or(self.start);
        self.len -= remaining;
        self
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

    /// Get the biggest level that can be used for mapping.
    pub fn biggest_level(&self) -> usize {
        let mut level = Table::top_level();
        while level > 0 {
            let size = Table::level_to_page_size(level);
            if self.start().is_aligned_to(size) && self.remaining() >= size {
                break;
            } else {
                level -= 1;
            }
        }
        level
    }
}

#[cfg(test)]
mod tests {
    use twizzler_kernel_macros::kernel_test;

    use super::*;
    use crate::memory::frame::PHYS_LEVEL_LAYOUTS;

    #[kernel_test]
    fn test_biggest_level() {
        let level_0_size = PHYS_LEVEL_LAYOUTS[0].size() as u64;
        let level_1_size = PHYS_LEVEL_LAYOUTS[1].size() as u64;
        let level_2_size = PHYS_LEVEL_LAYOUTS[2].size() as u64;
        let cursor = MappingCursor::new(
            VirtAddr::new(level_0_size * 4).unwrap(),
            level_0_size as usize * 3,
        );
        assert_eq!(cursor.biggest_level(), 0);
        let cursor = MappingCursor::new(
            VirtAddr::new(level_1_size * 4).unwrap(),
            level_1_size as usize * 2,
        );
        assert_eq!(cursor.biggest_level(), 1);
        let cursor = MappingCursor::new(
            VirtAddr::new(level_2_size * 4).unwrap(),
            level_2_size as usize * 4,
        );
        assert_eq!(cursor.biggest_level(), 2);
    }
}
