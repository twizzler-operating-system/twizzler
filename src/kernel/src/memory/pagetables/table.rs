use crate::{
    arch::{
        address::VirtAddr,
        pagetables::{Entry, EntryFlags, Table, PAGE_TABLE_ENTRIES},
    },
    memory::{frame::PhysicalFrameFlags, pagetables::MappingFlags},
};

use super::{
    consistency::Consistency, MapInfo, MappingCursor, MappingSettings, PhysAddrProvider, PhysFrame,
};

impl Table {
    pub fn set_count(&mut self, count: usize) {
        // NOTE: this function doesn't need cache line or TLB flushing because the hardware never reads these bits.
        for b in 0..16 {
            if count & (1 << b) == 0 {
                self[b].set_avail_bit(false);
            } else {
                self[b].set_avail_bit(true);
            }
        }
    }

    pub fn read_count(&self) -> usize {
        let mut count = 0;
        for b in 0..16 {
            let bit = self[b].get_avail_bit();
            count |= if bit { 1 } else { 0 } << b;
        }
        count
    }

    fn is_leaf(addr: VirtAddr, level: usize) -> bool {
        level == 0 || addr.is_aligned_to(1 << (12 + 9 * level))
    }

    fn get_index(addr: VirtAddr, level: usize) -> usize {
        let shift = 12 + 9 * level;
        (u64::from(addr) >> shift) as usize & 0x1ff
    }

    pub fn level_to_page_size(level: usize) -> usize {
        if level > 3 {
            panic!("invalid level");
        }
        1 << (12 + 9 * level)
    }

    pub fn next_table_mut(&mut self, index: usize) -> Option<&mut Table> {
        let entry = self[index];
        if entry.is_unused() || entry.is_huge() {
            return None;
        }
        let addr = entry.addr().kernel_vaddr();
        unsafe { Some(&mut *(addr.as_mut_ptr::<Table>())) }
    }

    pub fn next_table(&self, index: usize) -> Option<&Table> {
        let entry = self[index];
        if entry.is_unused() || entry.is_huge() {
            return None;
        }
        let addr = entry.addr().kernel_vaddr();
        unsafe { Some(&*(addr.as_ptr::<Table>())) }
    }

    fn can_map_at(vaddr: VirtAddr, paddr: PhysFrame, remain: usize, level: usize) -> bool {
        //logln!("==> {:?} {:?} {} {}", vaddr, paddr, remain, level);
        let page_size = Table::level_to_page_size(level);
        vaddr.is_aligned_to(page_size)
            && remain >= page_size
            && paddr.addr().is_aligned_to(page_size)
            && Self::can_map_at_level(level)
            && paddr.len() >= page_size
    }

    fn populate(&mut self, index: usize, flags: EntryFlags) {
        let count = self.read_count();
        let entry = &mut self[index];
        if entry.is_unused() {
            let frame = crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED);
            *entry = Entry::new(frame.start_address().as_u64().try_into().unwrap(), flags);
            self.set_count(count + 1);
        }
    }

    fn update_entry(
        &mut self,
        consist: &mut Consistency,
        index: usize,
        new_entry: Entry,
        vaddr: VirtAddr,
        was_terminal: bool,
        level: usize,
    ) {
        let count = self.read_count();
        let entry = &mut self[index];
        // TODO: check if we are doing a no-op, and early return

        let was_present = entry.is_present();
        let was_global = entry
            .flags()
            .settings()
            .flags()
            .contains(MappingFlags::GLOBAL);

        logln!("update entry {:x}", new_entry.raw());
        *entry = new_entry;
        let entry_addr = VirtAddr::from(entry);
        consist.flush(entry_addr);

        if was_present {
            consist.enqueue(vaddr, was_global, was_terminal, level)
        }

        if was_present && !new_entry.is_present() {
            self.set_count(count - 1);
        } else if !was_present && new_entry.is_present() {
            self.set_count(count + 1);
        } else {
            // TODO: we may be able to remove this write if we know we're not modifying entries whose avail bits we use.
            self.set_count(count);
        }
    }

    pub fn map(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];

            if entry.is_present() && (entry.is_huge() || level == 0) {
                phys.consume(Self::level_to_page_size(level));
                continue;
            }

            let paddr = phys.peek();

            if Self::can_map_at(cursor.start(), paddr, cursor.remaining(), level) {
                self.update_entry(
                    consist,
                    idx,
                    Entry::new(paddr.addr(), EntryFlags::from(settings)),
                    cursor.start(),
                    true,
                    level,
                );
                phys.consume(Self::level_to_page_size(level));
            } else {
                assert_ne!(level, 0);
                self.populate(idx, EntryFlags::intermediate());
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.map(consist, cursor, level - 1, phys, settings);
            }

            if let Some(next) = cursor.advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
    }

    // TODO: freeing
    pub fn unmap(&mut self, consist: &mut Consistency, mut cursor: MappingCursor, level: usize) {
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];

            if entry.is_present() && (entry.is_huge() || level == 0) {
                self.update_entry(
                    consist,
                    idx,
                    Entry::new_unused(),
                    cursor.start(),
                    true,
                    level,
                );
            } else if entry.is_present() && level != 0 {
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.unmap(consist, cursor, level - 1);
            }

            if let Some(next) = cursor.advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
    }

    pub fn change(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        settings: &MappingSettings,
    ) {
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];
            let is_present = entry.is_present();
            let is_huge = entry.is_huge();
            let addr = entry.addr();

            if is_present && (is_huge || level == 0) {
                self.update_entry(
                    consist,
                    idx,
                    Entry::new(addr, EntryFlags::from(settings)),
                    cursor.start(),
                    true,
                    level,
                );
            } else if is_present && level != 0 {
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.change(consist, cursor, level - 1, settings);
            }

            if let Some(next) = cursor.advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
    }

    pub fn readmap(&self, cursor: &MappingCursor, level: usize) -> Option<MapInfo> {
        let index = Self::get_index(cursor.start(), level);
        let entry = &self[index];
        if entry.is_present() && (entry.is_huge() || level == 0) {
            Some(MapInfo::new(
                cursor.start(),
                entry.addr(),
                entry.flags().settings(),
                Self::level_to_page_size(level),
            ))
        } else if entry.is_present() && level != 0 {
            let next_table = self.next_table(index).unwrap();
            next_table.readmap(cursor, level - 1)
        } else {
            None
        }
    }
}
