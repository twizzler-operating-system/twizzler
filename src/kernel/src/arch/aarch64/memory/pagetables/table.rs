use core::ops::{Index, IndexMut};

use arm64::registers::TTBR1_EL1;
use registers::interfaces::Readable;

use crate::{arch::address::VirtAddr, memory::PhysAddr};

use super::Entry;

#[repr(transparent)]
/// Representation of a page table. Can be indexed with [].
pub struct Table {
    entries: [Entry; Self::PAGE_TABLE_ENTRIES],
}

impl Table {
    /// The number of entries in this table.
    ///
    /// The number of entries on aarch64 depends on the translation granule size
    /// In this case we are going with a 4 KiB page size, so we have 512 entries
    /// at each level.
    pub const PAGE_TABLE_ENTRIES: usize = 512;

    /// The level of the last level page table.
    ///
    /// This depends on the translation granule size for aarch64.
    /// A 4 KiB translation size results in 4 level page tables (0-3)
    const MAX_LEVEL: usize = 3;

    /// The top level of the first page table in address translation.
    ///
    /// For 4 KiB pages, this means we start at stage 0/level 0 in the translation
    /// process. We assume that we are using 48-bits of address space.
    const TOP_LEVEL: usize = 0;

    /// The mask for indices encoded into a virtual address
    const INDEX_MASK: usize = 0x1FF;

    /// Get the current root table.
    pub fn current() -> PhysAddr {
        // Here we assume that we need the higher half of
        // the address space (kernel). This is only used to bootstrap
        // memory management. So we ignore TTBR0_EL1 which is for the
        // lower half.
        let ttbr1 = TTBR1_EL1.get();
        PhysAddr::new(ttbr1).unwrap()
    }

    /// The top level of a complete set of page tables.
    pub fn top_level() -> usize {
        Self::TOP_LEVEL
    }

    /// Does this system support mapping a huge page at this level?
    pub fn can_map_at_level(level: usize) -> bool {
        // huge pages, meaning larger than 4KiB (2MiB, 1GiB)
        // Seems like ARM does have huge pages, at certan levels
        match level {
            // check if TCR_EL0.DS is 1 (52-bit addr space)
            // if so then we can support 512 GiB at level 0

            // 1 GiB
            1 => true,
            // 2 MiB
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
        // for now let's make this a no-op
        // the pt entries on arm does have some spare bits
    }

    /// Read the current count of used entries.
    pub fn read_count(&self) -> usize {
        todo!("read count")
    }

    /// Is this a leaf (a huge page or page aligned) at a given level
    pub fn is_leaf(_addr: VirtAddr, _level: usize) -> bool {
        todo!("is_leaf")
    }

    /// Get the index for the next table for an address.
    pub fn get_index(addr: VirtAddr, level: usize) -> usize {
        // for a 4kib translation granule, a virtual address
        // is cut up int 5 pieces. This means that each
        // index is 9 address bits, with the first 12 bits
        // being a part of the block offset/physical address
        usize::from(addr) >> (9 * (Self::MAX_LEVEL - level) + 12) & Self::INDEX_MASK
    }

    /// Get the page size of a given level.
    pub fn level_to_page_size(level: usize) -> usize {
        // frame size * num entries ** (3-level)
        1 << (12 + 9 * (Self::MAX_LEVEL - level)) 
    }

    /// Get the level of the last page table.
    pub fn last_level() -> usize {
        Self::MAX_LEVEL
    }

    /// Get the value of the next level given the current level.
    pub fn next_level(level: usize) -> usize {
        // the levels of page tables on aarch64 begin with 0
        // and then increment from there
        level + 1
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
