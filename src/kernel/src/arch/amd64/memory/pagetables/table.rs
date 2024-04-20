use core::ops::{Index, IndexMut};

use super::Entry;
use crate::{arch::address::VirtAddr, memory::PhysAddr};

#[repr(transparent)]
/// Representation of a page table. Can be indexed with [].
pub struct Table {
    entries: [Entry; Self::PAGE_TABLE_ENTRIES],
}

impl Table {
    /// The number of entries in this table.
    pub const PAGE_TABLE_ENTRIES: usize = 512;

    /// The top level of a set of page tables.
    const TOP_LEVEL: usize = 3;

    /// The last level of a set of page tables.
    const LAST_LEVEL: usize = 0;

    /// Get the current root table.
    pub fn current() -> PhysAddr {
        let cr3 = unsafe { x86::controlregs::cr3() };
        PhysAddr::new(cr3).unwrap()
    }

    /// The top level of a complete set of page tables.
    pub fn top_level() -> usize {
        // TODO: support 5-level paging
        Self::TOP_LEVEL
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
    /// this function may choose to do something clever, like store the count in the available bits.
    /// But it could also make this function a no-op, and make [Table::read_count] just count
    /// the entries.
    pub fn set_count(&mut self, count: usize) {
        // NOTE: this function doesn't need cache line or TLB flushing because the hardware never
        // reads these bits.
        for b in 0..16 {
            if count & (1 << b) == 0 {
                self[b].set_avail_bit(false);
            } else {
                self[b].set_avail_bit(true);
            }
        }
    }

    /// Read the current count of used entries.
    pub fn read_count(&self) -> usize {
        let mut count = 0;
        for b in 0..16 {
            let bit = self[b].get_avail_bit();
            count |= usize::from(bit) << b;
        }
        count
    }

    /// Is this a leaf (a huge page or page aligned) at a given level
    pub fn is_leaf(addr: VirtAddr, level: usize) -> bool {
        level == Self::LAST_LEVEL || addr.is_aligned_to(1 << (12 + 9 * level))
    }

    /// Get the index for the next table for an address.
    pub fn get_index(addr: VirtAddr, level: usize) -> usize {
        let shift = 12 + 9 * level;
        (u64::from(addr) >> shift) as usize & 0x1ff
    }

    /// Get the page size of a given level.
    pub fn level_to_page_size(level: usize) -> usize {
        if level > Self::TOP_LEVEL {
            panic!("invalid level");
        }
        // frame size * (num entries) ^ level
        // 4096 * 512 ^ (level)
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
