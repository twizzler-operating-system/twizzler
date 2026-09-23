use core::ops::{Index, IndexMut};

use arm64::registers::TTBR1_EL1;
use registers::interfaces::Readable;

use super::Entry;
use crate::{arch::address::VirtAddr, memory::PhysAddr};

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

    /// Levels are numbered the way the generic page-table code counts them, from the leaf
    /// tables up: 0 is the 4 KiB page table (ARM's level 3) and 3 is the root (ARM's level 0),
    /// so `level - 1` descends and `PHYS_LEVEL_LAYOUTS[level]` is that level's page size.
    const LAST_LEVEL: usize = 0;
    const TOP_LEVEL: usize = 3;

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
        // 4 KiB pages, 2 MiB and 1 GiB blocks.
        level <= 2
    }

    /// Set the current count of used entries.
    ///
    /// Note: On some architectures that make available bits in the page table entries,
    /// this function may choose to do something clever, like store the count in the available bits.
    /// But it could also make this function a no-op, and make [Table::read_count] just count
    /// the entries.
    pub(crate) fn set_count_spread(&mut self, _count: usize) {
        // for now let's make this a no-op
        // the pt entries on arm does have some spare bits
    }

    /// Read the current count of used entries.
    pub(crate) fn read_count_spread(&self) -> usize {
        let mut count = 0;
        for entry in self.entries {
            if entry.is_present() {
                count += 1;
            }
        }
        count
    }

    /// Is this a leaf (a huge page or page aligned) at a given level
    pub fn is_leaf(addr: VirtAddr, level: usize) -> bool {
        level == Self::LAST_LEVEL || addr.is_aligned_to(Self::level_to_page_size(level))
    }

    /// Get the index for the next table for an address.
    pub fn get_index(addr: VirtAddr, level: usize) -> usize {
        // 4 KiB granule: 9 index bits per level above the 12-bit page offset.
        usize::from(addr) >> (9 * level + 12) & Self::INDEX_MASK
    }

    /// Get the page size of a given level.
    pub fn level_to_page_size(level: usize) -> usize {
        1 << (12 + 9 * level)
    }

    /// Get the level of the last page table.
    pub fn last_level() -> usize {
        Self::LAST_LEVEL
    }

    /// Get the value of the next level given the current level.
    pub fn next_level(level: usize) -> usize {
        level - 1
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
