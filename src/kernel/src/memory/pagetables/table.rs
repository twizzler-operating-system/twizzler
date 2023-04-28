use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        memory::pagetables::{Entry, EntryFlags, Table},
    },
    memory::{
        frame::{alloc_frame, get_frame, FrameRef, PhysicalFrameFlags},
        pagetables::MappingFlags,
    },
};

use super::{consistency::Consistency, MapInfo, MappingCursor, MappingSettings, PhysAddrProvider};

impl Table {
    fn next_table_mut(&mut self, index: usize) -> Option<&mut Table> {
        let entry = self[index];
        if !entry.is_present() || entry.is_huge() {
            return None;
        }
        let addr = entry.table_addr().kernel_vaddr();
        unsafe { Some(&mut *(addr.as_mut_ptr::<Table>())) }
    }

    fn next_table(&self, index: usize) -> Option<&Table> {
        let entry = self[index];
        if !entry.is_present() || entry.is_huge() {
            return None;
        }
        let addr = entry.table_addr().kernel_vaddr();
        unsafe { Some(&*(addr.as_ptr::<Table>())) }
    }

    fn next_table_frame(&self, index: usize) -> Option<FrameRef> {
        let entry = self[index];
        if !entry.is_present() || entry.is_huge() {
            return None;
        }
        let addr: u64 = entry.table_addr().into();
        get_frame(PhysAddr::new(addr).unwrap())
    }

    fn can_map_at(
        vaddr: VirtAddr,
        paddr: PhysAddr,
        remain: usize,
        phys_len: usize,
        level: usize,
    ) -> bool {
        let page_size = Table::level_to_page_size(level);
        vaddr.is_aligned_to(page_size)
            && remain >= page_size
            && paddr.is_aligned_to(page_size)
            && Self::can_map_at_level(level)
            && phys_len >= page_size
    }

    fn populate(&mut self, index: usize, flags: EntryFlags) {
        let count = self.read_count();
        let entry = &mut self[index];
        if !entry.is_present() {
            let frame = alloc_frame(PhysicalFrameFlags::ZEROED);
            *entry = Entry::new(frame.start_address(), flags);
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
        if *entry == new_entry {
            return;
        }

        let was_present = entry.is_present();
        let was_global = entry
            .flags()
            .settings()
            .flags()
            .contains(MappingFlags::GLOBAL);

        *entry = new_entry;
        let entry_addr = VirtAddr::from(entry as *const _);
        consist.flush(entry_addr);

        if was_present {
            consist.enqueue(vaddr, was_global, was_terminal, level)
        }

        if was_present && !new_entry.is_present() {
            self.set_count(count - 1);
        } else if !was_present && new_entry.is_present() {
            self.set_count(count + 1);
        } else {
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

            if entry.is_present() && (entry.is_huge() || level == Self::last_level()) {
                phys.consume(Self::level_to_page_size(level));
                if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                    cursor = next;
                } else {
                    break;
                }
                continue;
            }

            let paddr = phys.peek();
            if Self::can_map_at(cursor.start(), paddr.0, cursor.remaining(), paddr.1, level) {
                self.update_entry(
                    consist,
                    idx,
                    Entry::new(
                        paddr.0,
                        EntryFlags::from(settings)
                            | if level != Self::last_level() {
                                EntryFlags::huge()
                            } else {
                                EntryFlags::empty()
                            },
                    ),
                    cursor.start(),
                    true,
                    level,
                );
                phys.consume(Self::level_to_page_size(level));
            } else {
                assert_ne!(level, Self::last_level());
                self.populate(idx, EntryFlags::intermediate());
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.map(consist, cursor, Self::next_level(level), phys, settings);
            }

            if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
    }

    pub(super) fn unmap(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
    ) {
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];

            if entry.is_present() && (entry.is_huge() || level == Self::last_level()) {
                self.update_entry(
                    consist,
                    idx,
                    Entry::new_unused(),
                    cursor.start(),
                    true,
                    level,
                );
            } else if entry.is_present() && level != Self::last_level() {
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.unmap(consist, cursor, Self::next_level(level));
                if next_table.read_count() == 0 && level != Table::top_level() {
                    // Unwrap-Ok: The entry is present, and not a leaf, so it must be a table.
                    consist.free_frame(self.next_table_frame(idx).unwrap());
                    self.update_entry(
                        consist,
                        idx,
                        Entry::new_unused(),
                        cursor.start(),
                        false,
                        level,
                    );
                }
            }

            if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
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
            let addr = entry.addr(level);

            if is_present && (is_huge || level == Self::last_level()) {
                self.update_entry(
                    consist,
                    idx,
                    Entry::new(
                        addr,
                        EntryFlags::from(settings)
                            | if level != Self::last_level() {
                                EntryFlags::huge()
                            } else {
                                EntryFlags::empty()
                            },
                    ),
                    cursor.start(),
                    true,
                    level,
                );
            } else if is_present && level != Self::last_level() {
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.change(consist, cursor, Self::next_level(level), settings);
            }

            if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
    }

    pub(super) fn readmap(&self, cursor: &MappingCursor, level: usize) -> Result<MapInfo, usize> {
        let index = Self::get_index(cursor.start(), level);
        let entry = &self[index];
        if entry.is_present() && (entry.is_huge() || level == Self::last_level()) {
            Ok(MapInfo::new(
                cursor.start(),
                entry.addr(level),
                entry.flags().settings(),
                Self::level_to_page_size(level),
            ))
        } else if entry.is_present() && level != Self::last_level() {
            let next_table = self.next_table(index).unwrap();
            next_table.readmap(cursor, Self::next_level(level))
        } else {
            Err(Table::level_to_page_size(level))
        }
    }
}
