use crate::{
    arch::{
        address::VirtAddr,
        memory::pagetables::{Entry, EntryFlags, Table},
    },
    memory::{frame::PhysicalFrameFlags, pagetables::MappingFlags},
};

use super::{
    consistency::Consistency, MapInfo, MappingCursor, MappingSettings, PhysAddrProvider, PhysFrame,
};

impl Table {
    fn next_table_mut(&mut self, index: usize) -> Option<&mut Table> {
        let entry = self[index];
        if !entry.is_present() || entry.is_huge() {
            return None;
        }
        let addr = entry.addr().kernel_vaddr();
        unsafe { Some(&mut *(addr.as_mut_ptr::<Table>())) }
    }

    fn next_table(&self, index: usize) -> Option<&Table> {
        let entry = self[index];
        if !entry.is_present() || entry.is_huge() {
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
        if !entry.is_present() {
            let frame = crate::memory::alloc_frame(PhysicalFrameFlags::ZEROED);
            *entry = Entry::new(frame.start_address().as_u64().try_into().unwrap(), flags);
            // Synchronization with other TLBs 
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

    pub(super) fn map(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        phys: &mut impl PhysAddrProvider,
        settings: &MappingSettings,
    ) {
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
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
    pub(super) fn unmap(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
    ) {
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
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

    pub(super) fn change(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        settings: &MappingSettings,
    ) {
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
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

    pub(super) fn readmap(&self, cursor: &MappingCursor, level: usize) -> Option<MapInfo> {
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
