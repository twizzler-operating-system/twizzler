use core::ops::{Index, IndexMut};

use crate::{arch::address::VirtAddr, memory::PhysAddr};

use super::Entry;

#[repr(transparent)]
/// Representation of a page table. Can be indexed with [].
pub struct Table {
    entries: [Entry; Self::PAGE_TABLE_ENTRIES],
}

impl Table {
    // TODO:
    /// The number of entries in this table.
    pub const PAGE_TABLE_ENTRIES: usize = 2;

    /// Get the current root table.
    pub fn current() -> PhysAddr {
        todo!()
    }

    /// The top level of a complete set of page tables.
    pub fn top_level() -> usize {
        // TODO: support 5-level paging
        todo!()
    }

    /// Does this system support mapping a huge page at this level?
    pub fn can_map_at_level(level: usize) -> bool {
        match level {
            0 => true,
            1 => true,
            // TODO: check cpuid
            2 => true,
            _ => false,
        }
    }

    /// Set the current count of used entries.
    ///
    /// Note: On some architectures that make available bits in the page table entries,
    /// this function may choose to do something clever, like store the count in the available bits. But it could also
    /// make this function a no-op, and make [Table::read_count] just count the entries.
    pub fn set_count(&mut self, _count: usize) {
        // NOTE: this function doesn't need cache line or TLB flushing because the hardware never reads these bits.
        todo!()
    }

    /// Read the current count of used entries.
    pub fn read_count(&self) -> usize {
        todo!()
    }

    /// Is this a leaf (a huge page or page aligned) at a given level
    pub fn is_leaf(_addr: VirtAddr, _level: usize) -> bool {
        todo!()
    }

    /// Get the index for the next table for an address.
    pub fn get_index(_addr: VirtAddr, _level: usize) -> usize {
        todo!()
    }

    /// Get the page size of a given level.
    pub fn level_to_page_size(_level: usize) -> usize {
        todo!()
    }
}

impl Index<usize> for Table {
    type Output = Entry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}

impl IndexMut<usize> for Table {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.entries[index]
    }
}
