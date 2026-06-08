use super::{MapInfo, MappingCursor, MappingSettings, PhysAddrProvider, consistency::Consistency};
use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        memory::pagetables::{Entry, EntryFlags, Table},
    },
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, get_frame},
        pagetables::{Mapper, MappingFlags},
        tracker::{FrameAllocFlags, try_alloc_frame},
    },
};

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

    fn populate(&mut self, index: usize, flags: EntryFlags) -> Option<()> {
        let count = self.read_count();
        let entry = &mut self[index];
        if !entry.is_present() {
            let frame = try_alloc_frame(
                FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
                PHYS_LEVEL_LAYOUTS[0],
            )?;
            *entry = Entry::new(frame.start_address(), flags);
            self.set_count(count + 1);
        }
        Some(())
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

    pub(super) fn split_huge(&mut self, index: usize, level: usize) -> Option<()> {
        let entry = &mut self[index];
        if !entry.is_present() || !entry.is_huge() {
            return Some(());
        }
        assert_ne!(level, Self::last_level());
        let start_paddr = entry.addr(level);
        let flags = entry.flags();
        // TODO: this might generate spurious faults.
        self.populate(index, EntryFlags::intermediate())?;

        let next_table = self.next_table_mut(index).unwrap();

        for i in 0..Table::PAGE_TABLE_ENTRIES {
            let paddr = start_paddr
                .offset(i * Self::level_to_page_size(level))
                .ok()?;
            next_table[i] = Entry::new(paddr, flags - EntryFlags::huge());
        }
        Some(())
    }

    pub(super) fn do_cow_copy(&mut self, index: usize, level: usize) -> Option<()> {
        let entry = &mut self[index];
        if !entry.is_present() || !entry.is_cow() {
            return Some(());
        }

        let orig_paddr = entry.addr(level);
        let flags = entry.flags();

        let frame = try_alloc_frame(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        )?;
        frame.copy_contents_from_physaddr(0, orig_paddr, PHYS_LEVEL_LAYOUTS[0].size());

        if level != 0 {
            let next_table = self.next_table_mut(index).unwrap();
            for i in 0..Table::PAGE_TABLE_ENTRIES {
                next_table[i].set_cow(true);
            }
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        self[index] = Entry::new(
            frame.start_address(),
            (flags | EntryFlags::WRITE) - EntryFlags::COW,
        );

        Some(())
    }

    pub(super) fn cow_copy(
        &mut self,
        consist: &mut Consistency,
        cursor: &MappingCursor,
        level: usize,
    ) -> Option<()> {
        if level < cursor.biggest_level() {
            return Some(());
        }

        let index = Self::get_index(cursor.start(), level);
        if self[index].is_huge() && level != Self::last_level() {
            self.split_huge(index, level)?;
        } else {
            self.do_cow_copy(index, level)?;
        }

        if let Some(next) = self.next_table_mut(index) {
            next.cow_copy(consist, cursor, level - 1)?;
        }
        Some(())
    }

    pub(super) fn object_map(
        &mut self,
        consist: &mut Consistency,
        cursor: MappingCursor,
        level: usize,
        object_tables: &mut Mapper,
    ) -> Option<()> {
        let index = Self::get_index(cursor.start(), level);

        let max_level = object_tables.start_level() - 1;
        let target_level = cursor.biggest_level().min(max_level) + 1;

        if level == target_level {
            let paddr = object_tables.get_table_addr(level);
            let mut flags = EntryFlags::intermediate();
            flags.insert(EntryFlags::OBJECT_TABLE);
            self.update_entry(
                consist,
                index,
                Entry::new(paddr, flags),
                cursor.start(),
                false,
                level,
            );
            Some(())
        } else if level > target_level {
            assert_ne!(level, Self::last_level());
            self.populate(index, EntryFlags::intermediate())?;
            let next_table = self.next_table_mut(index).unwrap();
            next_table.object_map(consist, cursor, Self::next_level(level), object_tables);
            Some(())
        } else {
            panic!("tried to map within arch-tables for shared tables");
        }
    }

    pub(super) fn map(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        phys: &mut impl PhysAddrProvider,
    ) -> Option<()> {
        let start_index = Self::get_index(cursor.start(), level);

        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];
            let is_huge = entry.is_huge() && Self::can_map_at_level(level);
            let paddr = phys.peek()?;
            if entry.is_present() && (is_huge || level == Self::last_level()) {
                phys.consume(Self::level_to_page_size(level));
                if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                    cursor = next;
                } else {
                    break;
                }
                continue;
            }

            if Self::can_map_at(
                cursor.start(),
                paddr.addr,
                cursor.remaining(),
                paddr.len,
                level,
            ) {
                self.update_entry(
                    consist,
                    idx,
                    Entry::new(
                        paddr.addr,
                        EntryFlags::from(&paddr.settings)
                            | if level != Self::last_level() {
                                EntryFlags::huge()
                            } else {
                                EntryFlags::leaf()
                            },
                    ),
                    cursor.start(),
                    true,
                    level,
                );
                phys.consume(Self::level_to_page_size(level));
            } else {
                assert_ne!(level, Self::last_level());
                self.populate(idx, EntryFlags::intermediate())?;
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.map(consist, cursor, Self::next_level(level), phys);
            }

            if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
        Some(())
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
            let is_huge = entry.is_huge() && Self::can_map_at_level(level);
            if entry.is_present() && (is_huge || level == Self::last_level()) {
                self.update_entry(
                    consist,
                    idx,
                    Entry::new_unused(),
                    cursor.start(),
                    true,
                    level,
                );
            } else if entry.is_present() && level != Self::last_level() {
                if !entry.is_object_table() {
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
            let is_huge = entry.is_huge() && Self::can_map_at_level(level);
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
                                EntryFlags::leaf()
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
        let is_huge = entry.is_huge() && Self::can_map_at_level(level);
        if entry.is_present() && (is_huge || level == Self::last_level()) {
            Ok(MapInfo::new(
                cursor.start(),
                entry.addr(level),
                entry.flags().settings(),
                Self::level_to_page_size(level),
                entry.is_object_table(),
            ))
        } else if entry.is_present() && level != Self::last_level() {
            let next_table = self.next_table(index).unwrap();
            next_table.readmap(cursor, Self::next_level(level))
        } else {
            Err(Table::level_to_page_size(level))
        }
    }
}
