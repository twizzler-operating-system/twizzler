use object::macho::N_EXT;

use super::{MapInfo, MappingCursor, MappingSettings, PhysAddrProvider, consistency::Consistency};
use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        memory::pagetables::{Entry, EntryFlags, Table},
    },
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, PhysicalFrameFlags, get_frame, split_frame},
        pagetables::{DeferredUnmappingOps, Mapper, MappingFlags},
        tracker::{FrameAllocFlags, FrameAllocator, try_alloc_frame},
    },
};

const LOG_LEVEL: log::Level = log::Level::Info;

impl Table {
    pub(super) fn next_table_mut(&mut self, index: usize) -> Option<&mut Table> {
        let entry = self[index];
        if !entry.is_present() || entry.is_huge() {
            return None;
        }
        let paddr = entry.table_addr();
        let addr = entry.table_addr().kernel_vaddr();
        unsafe { Some(&mut *(addr.as_mut_ptr::<Table>())) }
    }

    pub(super) fn next_table(&self, index: usize) -> Option<&Table> {
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

    pub(super) fn populate(&mut self, index: usize, flags: EntryFlags) -> Option<()> {
        let count = self.read_count();
        let entry = &mut self[index];
        if !entry.is_present() {
            let frame = try_alloc_frame(
                FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
                PHYS_LEVEL_LAYOUTS[0],
            )?;
            frame.set_pt(true);
            frame.inc_refcount();
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

        // TODO: do we need to decrement the page refcount, etc?

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
        if !entry.is_present() || !entry.is_huge() || level == 0 {
            return Some(());
        }
        assert_ne!(level, Self::last_level());
        let start_paddr = entry.addr(level);
        let large_frame = get_frame(start_paddr).unwrap();
        assert!(large_frame.size() == Self::level_to_page_size(level));
        todo!("split_huge: handle refcounting and COW for huge pages");
        split_frame(large_frame);
        let flags = entry.flags();
        // TODO: this might generate spurious faults.
        self[index].clear();
        self.populate(index, EntryFlags::intermediate())?;

        let next_table = self.next_table_mut(index).unwrap();

        for i in 0..Table::PAGE_TABLE_ENTRIES {
            let paddr = start_paddr
                .offset(i * Self::level_to_page_size(level))
                .ok()?;
            let frame = get_frame(paddr).unwrap();
            frame.inc_refcount();
            next_table[i] = Entry::new(paddr, flags - EntryFlags::huge());
        }
        Some(())
    }

    pub(super) fn do_cow_copy(&mut self, index: usize, level: usize) -> Option<()> {
        let entry = &mut self[index];
        let frame = get_frame(entry.addr(level));
        if frame.is_none() {
            // TODO: this would only happen for untracked frames, but we'd still like to copy them.
            //log::warn!("do_cow_copy: no frame for entry at level {}!", level);
            return Some(());
        }
        let frame = frame.unwrap();
        if !entry.is_present() || !frame.is_cow() {
            return Some(());
        }
        assert!(!entry.is_huge() || level == Self::last_level());

        let flags = entry.flags();
        let alloc = &mut FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let orig_frame = frame;
        let frame = frame.cow_frame(alloc);
        if frame.is_none() {
            log::warn!("failed to allocate frame for COW copy at level {}", level);
            return None;
        }
        let frame = frame?;

        log::log!(
            LOG_LEVEL,
            "do_cow_copy: level {}, index {}, orig_frame {:x}, new_frame {:x}, flags {:?}",
            level,
            index,
            orig_frame.start_address().raw(),
            frame.start_address().raw(),
            flags
        );

        if frame.start_address() == orig_frame.start_address() {
            return Some(());
        }

        if level != 0 {
            assert!(frame.is_pt());
            let next_table = self.next_table_mut(index).unwrap();
            for i in 0..Table::PAGE_TABLE_ENTRIES {
                let next_frame = get_frame(next_table[i].addr(level - 1)).unwrap();
                let new_flags = next_table[i].flags() - EntryFlags::WRITE;
                next_table[i].set_flags(new_flags);
                next_frame.set_cow(true);
                next_frame.inc_refcount();
            }
        } else {
            assert!(!frame.is_pt());
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        self[index] = Entry::new(frame.start_address(), flags | EntryFlags::WRITE);

        Some(())
    }

    pub(super) fn cow_copy(
        &mut self,
        consist: &mut Consistency,
        cursor: &MappingCursor,
        level: usize,
    ) -> Option<bool> {
        if level < cursor.biggest_level() {
            return Some(false);
        }

        let index = Self::get_index(cursor.start(), level);
        let mut did_cow = false;
        if self[index].is_huge() && level != Self::last_level() {
            self.split_huge(index, level)?;
        } else {
            self.do_cow_copy(index, level)?;
            did_cow = true;
        }

        if level > 0 {
            if let Some(next) = self.next_table_mut(index) {
                did_cow |= next.cow_copy(consist, cursor, level - 1)?;
            }
        }
        Some(did_cow)
    }

    pub(super) fn object_map(
        &mut self,
        consist: &mut Consistency,
        cursor: MappingCursor,
        level: usize,
        object_tables: &mut Mapper,
    ) -> Option<()> {
        let index = Self::get_index(cursor.start(), level);

        let max_level = object_tables.start_level();
        let target_level = cursor.biggest_level().min(max_level);

        if level == target_level + 1 {
            let paddr = object_tables.get_table_addr(target_level);
            let frame = get_frame(paddr).unwrap();
            frame.inc_refcount();
            assert!(frame.is_pt());
            log::log!(
                LOG_LEVEL,
                "object_map: mapping object table at level {} to paddr {:x}",
                level,
                paddr
            );
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
        } else if level > target_level + 1 {
            assert_ne!(level, Self::last_level());
            self.populate(index, EntryFlags::intermediate())?;
            let next_table = self.next_table_mut(index).unwrap();
            next_table.object_map(consist, cursor, Self::next_level(level), object_tables);
            Some(())
        } else {
            panic!("tried to map within arch-tables for shared tables");
        }
    }

    pub(super) fn split_to_level(
        &mut self,
        consist: &mut Consistency,
        addr: VirtAddr,
        current_level: usize,
        target_level: usize,
    ) -> Option<()> {
        if target_level == current_level {
            return Some(());
        }
        let index = Self::get_index(addr, current_level);

        if let Some(frame) = self.next_table_frame(index) {
            if frame.is_cow() {
                self.do_cow_copy(index, current_level)?;
            }
        }

        if let Some(next_table) = self.next_table_mut(index) {
            return next_table.split_to_level(consist, addr, current_level - 1, target_level);
        }

        if self[index].is_present() && self[index].is_huge() {
            self.split_huge(index, current_level)?;
        } else if !self[index].is_present() {
            self.populate(index, EntryFlags::intermediate())?;
        }

        self.next_table_mut(index).unwrap().split_to_level(
            consist,
            addr,
            current_level - 1,
            target_level,
        )?;

        Some(())
    }

    pub fn setup_cow_range(
        &mut self,
        dest: &mut Self,
        src_cursor: &mut MappingCursor,
        dst_cursor: &mut MappingCursor,
        level: usize,
    ) -> Option<()> {
        assert!(src_cursor.remaining() == dst_cursor.remaining());

        let src_start_index = Self::get_index(src_cursor.start(), level);
        let dst_start_index = Self::get_index(dst_cursor.start(), level);
        let count = Self::PAGE_TABLE_ENTRIES - src_start_index.max(dst_start_index);

        log::log!(
            LOG_LEVEL,
            "setup_cow_range: level {}, src_start_index {}, dst_start_index {}, count {}",
            level,
            src_start_index,
            dst_start_index,
            count
        );

        for i in 0..count {
            if src_cursor.remaining() == 0 || dst_cursor.remaining() == 0 {
                break;
            }
            let src_index = src_start_index + i;
            let dst_index = dst_start_index + i;

            let src_entry = &mut self[src_index];

            if !src_entry.is_present() {
                *src_cursor = src_cursor.advance_until_empty(Self::level_to_page_size(level));
                *dst_cursor = dst_cursor.advance_until_empty(Self::level_to_page_size(level));
                continue;
            }

            let is_aligned = src_cursor
                .start()
                .is_aligned_to(Self::level_to_page_size(level))
                && dst_cursor
                    .start()
                    .is_aligned_to(Self::level_to_page_size(level));

            if !is_aligned {
                // TODO: is this safe?
                log::trace!(
                    "not aligned for this level: src_cursor {:?}, dst_cursor {:?}, level {}",
                    src_cursor,
                    dst_cursor,
                    level
                );
                assert!(!src_entry.is_huge());
                dest.populate(dst_index, EntryFlags::intermediate())?;

                let next_dest_table = dest.next_table_mut(dst_index).unwrap();
                let next_src_table = self.next_table_mut(src_index).unwrap();
                next_src_table.setup_cow_range(
                    next_dest_table,
                    src_cursor,
                    dst_cursor,
                    level - 1,
                )?;
                continue;
            }

            let src_flags = src_entry.flags();
            let src_addr = src_entry.addr(level);
            let src_frame = get_frame(src_addr).unwrap();
            src_frame.inc_refcount();
            src_frame.set_cow(true);
            log::log!(
                LOG_LEVEL,
                "copying entry without write: src_index {}, dst_index {}, level {}, src_addr {:x}, src_flags {:?}",
                src_index,
                dst_index,
                level,
                src_addr,
                src_flags
            );

            dest[dst_index] = Entry::new(src_addr, src_flags - EntryFlags::WRITE);
            self[src_index] = Entry::new(src_addr, src_flags - EntryFlags::WRITE);

            *src_cursor = src_cursor.advance_until_empty(Self::level_to_page_size(level));
            *dst_cursor = dst_cursor.advance_until_empty(Self::level_to_page_size(level));
        }

        Some(())
    }

    pub(super) fn map(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        phys: &mut impl PhysAddrProvider,
    ) -> Option<()> {
        let start_index = Self::get_index(cursor.start(), level);
        log::trace!(
            "map: level {}, start_index {}, cursor {:?}",
            level,
            start_index,
            cursor
        );

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
                if let Some(frame) = get_frame(paddr.addr)
                    && !paddr.settings.flags().contains(MappingFlags::WIRED)
                {
                    log::trace!(
                        "map: mapping frame {:x} at level {} for vaddr {:x} with flags {:?}",
                        frame.start_address().raw(),
                        level,
                        cursor.start().raw(),
                        frame.get_flags()
                    );
                    assert!(!frame.is_pt());
                    frame.inc_refcount();
                }
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
                if self.next_table_frame(idx).is_some_and(|f| f.is_cow()) {
                    self.do_cow_copy(idx, level);
                }
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
                let frame = get_frame(entry.addr(level));
                let flags = entry.flags();
                self.update_entry(
                    consist,
                    idx,
                    Entry::new_unused(),
                    cursor.start(),
                    true,
                    level,
                );
                if let Some(frame) = frame
                    && !flags.contains(EntryFlags::WIRED)
                {
                    assert!(!frame.is_pt());
                    log::log!(
                        LOG_LEVEL,
                        "unmap: freeing frame {:x} at level {} for vaddr {:x} with flags {:?} (entry flags {:?})",
                        frame.start_address().raw(),
                        level,
                        cursor.start().raw(),
                        frame.get_flags(),
                        flags,
                    );
                    consist.free_frame(frame);
                }
            } else if entry.is_present() && level != Self::last_level() {
                if !entry.is_object_table() {
                    if self.next_table_frame(idx).is_some_and(|f| f.is_cow()) {
                        self.do_cow_copy(idx, level);
                    }
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
                } else {
                    get_frame(entry.table_addr()).unwrap().dec_refcount();
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
                if self.next_table_frame(idx).is_some_and(|f| f.is_cow()) {
                    self.do_cow_copy(idx, level);
                }
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

    pub(super) fn is_object_mapped(&self, cursor: MappingCursor, level: usize) -> bool {
        let index = Self::get_index(cursor.start(), level);
        let entry = &self[index];
        if entry.is_present() && entry.is_object_table() {
            return true;
        } else if entry.is_present() && level != Self::last_level() {
            let next_table = self.next_table(index).unwrap();
            return next_table.is_object_mapped(cursor, Self::next_level(level));
        }
        false
    }

    pub(super) fn readmap(&self, cursor: &MappingCursor, level: usize) -> Result<MapInfo, usize> {
        let index = Self::get_index(cursor.start(), level);
        let entry = &self[index];
        let is_huge = entry.is_huge() && Self::can_map_at_level(level);
        if entry.is_present() && (is_huge || level == Self::last_level()) {
            Ok(MapInfo::new(
                cursor
                    .start()
                    .align_down(Self::level_to_page_size(level) as u64)
                    .unwrap(),
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

    pub(super) fn print_tables_recursive(&self, level: usize, vaddr: VirtAddr, indent: usize) {
        for i in 0..Table::PAGE_TABLE_ENTRIES {
            let entry = &self[i];
            if entry.is_present() {
                let entry_vaddr = vaddr.offset(i * Table::level_to_page_size(level)).unwrap();
                let frame = get_frame(entry.addr(level));
                log::info!(
                    "{:indent$}[{:3}] {:16x} -> {:16x} {:?} {}:: refcount={}, {:?}",
                    "",
                    i,
                    entry_vaddr.raw(),
                    entry.addr(level),
                    entry.flags(),
                    if entry.is_huge() { "HUGE " } else { "" },
                    frame.as_ref().map(|f| f.refcount()).unwrap_or(0),
                    frame
                        .as_ref()
                        .map(|f| f.get_flags())
                        .unwrap_or(PhysicalFrameFlags::empty()),
                );
                if !entry.is_huge() && level != Table::last_level() {
                    let next_table = self.next_table(i).unwrap();
                    next_table.print_tables_recursive(
                        Self::next_level(level),
                        entry_vaddr,
                        indent + 2,
                    );
                }
            }
        }
    }
}
