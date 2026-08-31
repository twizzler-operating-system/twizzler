use twizzler_rt_abi::error::{ResourceError, TwzError};

use super::{MapInfo, MappingCursor, MappingSettings, PhysAddrProvider, consistency::Consistency};
use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        memory::pagetables::{Entry, EntryFlags, Table},
    },
    memory::{
        frame::{Frame, FrameRef, PHYS_LEVEL_LAYOUTS, PhysicalFrameFlags, get_frame, split_frame},
        pagetables::{Mapper, MappingFlags, zeroprobe},
        tracker::{FrameAllocFlags, FrameAllocator, try_alloc_frame},
    },
};

const LOG_LEVEL: log::Level = log::Level::Debug;

/// Take the frame `Table::map` installs from the provider when it already has it, instead of
/// looking it up by physical address.
///
/// See [`super::PhysMapInfo::frame`]. Off, this is `get_frame(paddr.addr)` exactly as before, so
/// the pair (`FRAME_LOOKUP_FAST`, this) attributes cleanly: `W_COW_GF_NS` moves only with the
/// first, `W_LEAF_GF_NS` with both.
const PROVIDER_CARRIES_FRAME: bool = true;

/// Large pages taken apart again.
///
/// Every split undoes a large page, so this is the drain against which any large-page win has to be
/// read: a boot that makes hundreds of them and splits hundreds is standing still. `cow_copy` used
/// to split unconditionally on any write path, which made that the normal outcome.
mod splits {
    use core::sync::atomic::{AtomicU64, Ordering};

    static SPLITS: AtomicU64 = AtomicU64::new(0);

    pub fn record() {
        let n = SPLITS.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_power_of_two() && crate::kdiag_pager() {
            log::info!("SPLITS: {} large pages split back to 4 KiB", n);
        }
    }
}

impl Table {
    pub(super) fn next_table_mut(&mut self, index: usize) -> Option<&mut Table> {
        let entry = self[index];
        if !entry.is_present() || entry.is_huge() {
            return None;
        }
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

    pub(super) fn populate(
        &mut self,
        index: usize,
        flags: EntryFlags,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        let count = self.read_count();
        let entry = &mut self[index];
        if !entry.is_present() {
            crate::obj::pagetables::mapprobe::tick(&crate::obj::pagetables::mapprobe::POPULATED);
            let frame = fa.try_allocate().ok_or(ResourceError::OutOfMemory)?;
            assert!(frame.size() == PHYS_LEVEL_LAYOUTS[0].size());
            // The 512 entries below are never written here, so they must already be zero.
            crate::memory::frame::ensure_pt_zeroed(frame, "populate");
            frame.set_pt(true);
            frame.inc_refcount();
            *entry = Entry::new(frame.start_address(), flags);
            self.set_count(count + 1);
        }
        Ok(())
    }

    /// Whether this entry is a mapping rather than a link to a lower table.
    ///
    /// Mirrors `readmap`'s condition for reporting a `MapInfo` exactly -- including that `is_huge`
    /// only means "huge" at a level that can map, since on x86 the same bit is PAT at the last
    /// level. If the two ever diverge, `count_pages` and the counter would disagree, which is what
    /// `COUNT_PAGES_VERIFY` exists to catch.
    fn entry_is_leaf(entry: &Entry, level: usize) -> bool {
        entry.is_present()
            && ((entry.is_huge() && Self::can_map_at_level(level)) || level == Self::last_level())
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
        let was_leaf = Self::entry_is_leaf(entry, level);
        let was_global = entry
            .flags()
            .settings()
            .flags()
            .contains(MappingFlags::GLOBAL);
        // TODO: do we need to decrement the page refcount, etc?

        *entry = new_entry;
        let entry_addr = VirtAddr::from(entry as *const _);
        consist.add_cache_line(entry_addr);

        // Skipping this for a same-frame permission *widening* is sound on x86 -- a stale, more
        // restrictive entry just takes a spurious fault and re-walks -- and was measured: it fires
        // twice per boot, because `do_cow_copy` reaches an entry update only for a frame that is
        // already IS_COW, and this workload barely clones. Not worth a fast path through the
        // hottest page-table function. See TLB.md.
        if was_present {
            consist.enqueue(vaddr, was_global, was_terminal, level)
        }

        // Page accounting, in the one place every entry write passes through. A leaf is exactly
        // what `readmap` reports as a mapping, so this and `count_pages` count the same thing by
        // construction. Non-leaf changes (installing an intermediate table) move no pages.
        let new_leaf = Self::entry_is_leaf(&new_entry, level);
        if was_leaf != new_leaf {
            let units = (Self::level_to_page_size(level)
                / Self::level_to_page_size(Self::last_level())) as isize;
            consist.add_page_delta(if new_leaf { units } else { -units });
        }

        if was_present && !new_entry.is_present() {
            self.set_count(count - 1);
        } else if !was_present && new_entry.is_present() {
            self.set_count(count + 1);
        } else {
            self.set_count(count);
        }
    }

    pub(super) fn from_frame_mut<'a>(frame: &'a Frame) -> &'a mut Table {
        assert!(frame.is_pt());
        let vaddr = frame.start_address().kernel_vaddr();
        unsafe { &mut *(vaddr.as_mut_ptr::<Table>()) }
    }

    pub(super) fn split_huge(
        &mut self,
        index: usize,
        level: usize,
        consist: &mut Consistency,
        vaddr: VirtAddr,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        let entry = &mut self[index];
        if !entry.is_present() || !entry.is_huge() || level == 0 {
            return Ok(());
        }
        assert_ne!(level, Self::last_level());
        let start_paddr = entry.addr(level);
        let large_frame = get_frame(start_paddr).unwrap();
        let flags = entry.flags();
        assert!(large_frame.size() == Self::level_to_page_size(level));
        splits::record();

        let new_table_frame = fa.try_allocate().ok_or(ResourceError::OutOfMemory)?;
        // Every entry *is* written below, but only on the paths that complete; an early return
        // between here and there would leave a table of whatever this frame arrived holding.
        crate::memory::frame::ensure_pt_zeroed(new_table_frame, "split_page");
        new_table_frame.set_pt(true);
        new_table_frame.inc_refcount();

        let next_table = Self::from_frame_mut(new_table_frame);
        if entry.flags().contains(EntryFlags::WRITE) {
            let (_, len) = split_frame(large_frame);
            assert!(
                len == Self::level_to_page_size(level),
                "split_frame returned unexpected length: {}",
                len
            );

            for i in 0..Table::PAGE_TABLE_ENTRIES {
                if let Ok(paddr) = start_paddr.offset(i * Self::level_to_page_size(level - 1)) {
                    if let Some(_frame) = get_frame(paddr) {
                        next_table[i] = Entry::new(paddr, flags - EntryFlags::huge());
                    }
                }
            }
        } else {
            // We can't split, so allocate and copy.
            for i in 0..Table::PAGE_TABLE_ENTRIES {
                // Don't use the frame allocator for this, since we want specific levels.
                let frame = try_alloc_frame(
                    FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
                    PHYS_LEVEL_LAYOUTS[level - 1],
                );
                if frame.is_none() {
                    consist.free_frame(new_table_frame);
                    for j in 0..i {
                        if next_table[j].is_present()
                            && let Some(frame) = get_frame(next_table[j].addr(level - 1))
                        {
                            consist.free_frame(frame);
                        }
                    }
                    return Err(ResourceError::OutOfMemory.into());
                }
                let frame = frame.unwrap();
                frame.copy_contents_from_physaddr(
                    0,
                    start_paddr
                        .offset(i * Table::level_to_page_size(level - 1))
                        .unwrap(),
                    frame.size(),
                );
                frame.inc_refcount();
                // Write access comes back with the copy. This branch runs because the entry was
                // read-only, which for an object table means COW -- and copying the contents into
                // private frames *is* the copy-on-write. Carrying the read-only flags across leaves
                // the faulting write with a private page it still cannot write and nothing left to
                // resolve, so it refaults forever. Only reachable at all once large pages survive
                // long enough to be COW'd.
                next_table[i] = Entry::new(
                    frame.start_address(),
                    (flags - EntryFlags::huge()) | EntryFlags::WRITE,
                );
            }
            consist.free_frame(large_frame);
        }
        next_table.set_count(Table::PAGE_TABLE_ENTRIES);

        let new_entry = Entry::new(new_table_frame.start_address(), EntryFlags::intermediate());
        self.update_entry(consist, index, new_entry, vaddr, true, level);

        Ok(())
    }

    pub(super) fn do_cow_copy(
        &mut self,
        index: usize,
        level: usize,
        consist: &mut Consistency,
        vaddr: VirtAddr,
        mark_dirty: bool,
        fa: &mut FrameAllocator,
    ) -> Result<bool, TwzError> {
        let entry = &mut self[index];
        if !entry.is_present() {
            return Ok(false);
        }
        let frame = get_frame(entry.addr(level));
        if frame.is_none() {
            // TODO: this would only happen for untracked frames, but we'd still like to copy them
            // maybe?
            return Ok(false);
        }
        let frame = frame.unwrap();
        if !frame.is_cow() {
            return Ok(false);
        }
        assert!(!entry.is_huge() || level == Self::last_level());

        let flags = entry.flags();
        let orig_frame = frame;
        let frame = frame.cow_frame(fa);
        if frame.is_none() {
            log::warn!("failed to allocate frame for COW copy at level {}", level);
            return Err(ResourceError::OutOfMemory.into());
        }
        let frame = frame.ok_or(ResourceError::OutOfMemory)?;

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
            let entry = Entry::new(frame.start_address(), flags | EntryFlags::WRITE);
            self.update_entry(consist, index, entry, vaddr, true, level);
            return Ok(false);
        }

        if level != 0 {
            assert!(frame.is_pt());
            let next_table = self.next_table_mut(index).unwrap();
            let mut downgraded = 0usize;
            for i in 0..Table::PAGE_TABLE_ENTRIES {
                if next_table[i].is_present() {
                    downgraded += 1;
                    let new_flags = next_table[i].flags() - EntryFlags::WRITE;
                    next_table[i].set_flags(new_flags);
                    // TODO: this is too many flushes.
                    consist.add_cache_line(VirtAddr::from_ptr(&next_table[i]));
                    if let Some(next_frame) = get_frame(next_table[i].addr(level - 1)) {
                        next_frame.set_cow(true);
                        next_frame.inc_refcount();
                    }
                }
            }
            // The loop above cleared WRITE across every present entry of this sub-table. A single
            // `invlpg` for `vaddr` covers one page of that, and at level > 1 each downgraded entry
            // is an intermediate covering 512 further pages, which no address list can express --
            // so invalidate the target wholesale. Measured at ~11 calls per boot, so the breadth
            // costs nothing; the previous single enqueue left up to 511 stale *writable* entries on
            // frames that had just become COW-shared, which is a write landing in the wrong object.
            //
            // Global, not just full, so the breadth is decided here rather than two frames away.
            // Every caller that reaches this arm today already ends up machine-wide --
            // `send_consistency` replaces a FULL object-side batch with a fresh full+global one,
            // and `lock_with_consist` starts kernel-range arch batches that way -- so
            // this is behaviour-neutral. It is written locally anyway because the
            // alternative is depending on that escalation, on `ArchContext::change`
            // staying uncalled, and on `unmap`'s `is_object_table` guard being
            // reproduced in any future descent. See TLB.md.
            consist.set_full_global();
            nonleaf_cow::record(downgraded);
        } else {
            assert!(!frame.is_pt());
        }
        // No fence here. The ordering between the downgrade loop above and the entry update below
        // is already established by the loop's `inc_refcount`, which is a `fetch_add(SeqCst)` and
        // so a full barrier on x86 -- and where an entry resolved no frame, by `clflush`,
        // which is ordered with respect to writes. That second leg is the fragile one:
        // CLFLUSHOPT is weakly ordered and would need an explicit SFENCE, so a later
        // conversion of `ArchCacheLineMgr` to `clflushopt` must restore a fence here. It is
        // `clflush` specifically that makes this safe, not "x86 orders stores".
        //
        // **That last sentence is now contradicted deliberately, not by accident.** On amd64
        // `PT_CLFLUSH` is off, so the `clflush` leg is gone entirely; what carries the ordering
        // there is exactly the thing the sentence rules out. x86-TSO does not reorder stores with
        // stores, so the downgrade loop's writes are visible before the entry write below to every
        // coherent observer, and on x86 the page-table walker is one. The sentence is right about
        // `clflush` being ordered and CLFLUSHOPT not being; it is wrong that store ordering alone
        // is insufficient *on this architecture*. The fragility it names is real for aarch64,
        // whose walker may not be coherent and whose `ArchCacheLineMgr` still issues
        // `dc cvac; dsb ishst; isb` -- which is why the gate is amd64-local and this call site is
        // unchanged. If the flush ever comes back on amd64 as `clflushopt`, restore a fence here.
        let new_flags = flags
            | EntryFlags::WRITE
            | if mark_dirty {
                EntryFlags::DIRTY
            } else {
                EntryFlags::empty()
            };

        self.update_entry(
            consist,
            index,
            Entry::new(frame.start_address(), new_flags),
            vaddr,
            true,
            level,
        );

        Ok(true)
    }

    pub(super) fn cow_copy(
        &mut self,
        consist: &mut Consistency,
        cursor: &MappingCursor,
        level: usize,
        mark_dirty: bool,
        fa: &mut FrameAllocator,
    ) -> Result<bool, TwzError> {
        log::log!(
            LOG_LEVEL,
            "cow_copy: cursor {:?}, level {}, biggest_level {}",
            cursor,
            level,
            cursor.biggest_level()
        );

        let index = Self::get_index(cursor.start(), level);
        let mut did_cow = false;
        if !self[index].is_present() {
            return Ok(false);
        }
        if self[index].is_huge() && level != Self::last_level() {
            // A writable entry over a frame nobody shares has no copy to make, and splitting it
            // only demotes the page: every write path reaches here through `maybe_cow_at` --
            // `write_bytes`, `set_bytes`, and every atomic through `with_ref` -- so splitting
            // unconditionally means no large page survives being written to.
            //
            // Anything else still splits. A read-only entry means a COW is in progress or has just
            // been resolved, and `do_cow_copy` (which asserts against a huge entry) is what
            // restores write access, so short-circuiting there would leave a write fault with
            // nothing to fix and no way to make progress.
            let writable = self[index].flags().contains(EntryFlags::WRITE);
            let shared = get_frame(self[index].addr(level)).is_some_and(|frame| frame.is_cow());
            if writable && !shared {
                return Ok(false);
            }
            self.split_huge(index, level, consist, cursor.start(), fa)?;
        } else {
            did_cow |= self.do_cow_copy(index, level, consist, cursor.start(), mark_dirty, fa)?;
        }

        if level > 0 {
            if let Some(next) = self.next_table_mut(index) {
                did_cow |= next.cow_copy(consist, cursor, level - 1, mark_dirty, fa)?;
            }
        }
        Ok(did_cow)
    }

    /// Map an object's page tables into these tables. Returns true if a new reference to the
    /// object table was taken, and false if an existing mapping was updated in place (e.g. a
    /// permissions upgrade), so callers can keep the object's map count symmetric with unmap.
    pub(super) fn object_map(
        &mut self,
        consist: &mut Consistency,
        cursor: MappingCursor,
        level: usize,
        object_tables: &mut Mapper,
        fa: &mut FrameAllocator,
        settings: MappingSettings,
    ) -> Result<bool, TwzError> {
        let index = Self::get_index(cursor.start(), level);

        let max_level = object_tables.start_level();
        let target_level = cursor.biggest_level().min(max_level);

        if level == target_level {
            // Get the next table down to map into an entry.
            let paddr = object_tables.get_table_addr(target_level - 1, fa)?;
            let frame = get_frame(paddr).unwrap();
            assert!(frame.is_pt());
            log::log!(
                LOG_LEVEL,
                "object_map: mapping object table at level {} to paddr {:x}",
                level,
                paddr
            );

            // If this object table is already mapped here, reuse its reference rather than taking
            // a second one that unmap would never release (this is the common case for a
            // permissions upgrade). Otherwise take a reference, releasing any table we displace.
            let old = self[index];
            let old_frame = if old.is_present() && old.is_object_table() {
                get_frame(old.table_addr())
            } else {
                None
            };
            let already_mapped = old_frame.is_some_and(|old| old.start_address() == paddr);
            if !already_mapped {
                frame.inc_refcount();
                if let Some(old_frame) = old_frame {
                    // Shouldn't happen: the slot should have been unmapped first, and whichever
                    // object owns old_frame now has a stale map count.
                    log::warn!(
                        "object_map: replacing object table {:x} with {:x} at level {}",
                        old_frame.start_address().raw(),
                        paddr,
                        level
                    );
                    old_frame.dec_refcount();
                }
            }
            let took_ref = !already_mapped;

            let mut flags = EntryFlags::intermediate();
            flags.apply_perms(settings.perms());
            // TODO: set cache type
            flags.insert(EntryFlags::OBJECT_TABLE);
            self.update_entry(
                consist,
                index,
                Entry::new(paddr, flags),
                cursor.start(),
                false,
                level,
            );
            Ok(took_ref)
        } else if level > target_level {
            assert_ne!(level, Self::last_level());
            self.populate(index, EntryFlags::intermediate(), fa)?;
            let next_table = self.next_table_mut(index).unwrap();
            next_table.object_map(
                consist,
                cursor,
                Self::next_level(level),
                object_tables,
                fa,
                settings,
            )
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
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        if target_level == current_level {
            return Ok(());
        }
        let index = Self::get_index(addr, current_level);

        if let Some(frame) = self.next_table_frame(index) {
            if frame.is_cow() {
                self.do_cow_copy(index, current_level, consist, addr, false, fa)?;
            }
        }

        if let Some(next_table) = self.next_table_mut(index) {
            return next_table.split_to_level(consist, addr, current_level - 1, target_level, fa);
        }

        if self[index].is_present() && self[index].is_huge() {
            self.split_huge(index, current_level, consist, addr, fa)?;
        } else if !self[index].is_present() {
            self.populate(index, EntryFlags::intermediate(), fa)?;
        }

        self.next_table_mut(index).unwrap().split_to_level(
            consist,
            addr,
            current_level - 1,
            target_level,
            fa,
        )?;

        Ok(())
    }

    pub fn setup_cow_range(
        &mut self,
        dest: &mut Self,
        src_cursor: &mut MappingCursor,
        dst_cursor: &mut MappingCursor,
        level: usize,
        consist: &mut Consistency,
        max_level: usize,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        assert!(src_cursor.remaining() == dst_cursor.remaining());

        let src_start_index = Self::get_index(src_cursor.start(), level);
        let dst_start_index = Self::get_index(dst_cursor.start(), level);
        let count = Self::PAGE_TABLE_ENTRIES - src_start_index.max(dst_start_index);
        log::trace!(
            "setup_cow_range: level {}, src_start_index {}, dst_start_index {}, count {} ({} remaining at this level), cursors = src {:?}, dst {:?}",
            level,
            src_start_index,
            dst_start_index,
            count,
            src_cursor.remaining() / Self::level_to_page_size(level),
            src_cursor,
            dst_cursor
        );
        for _ in 0..count {
            if src_cursor.remaining() == 0 || dst_cursor.remaining() == 0 {
                break;
            }
            let src_index = Self::get_index(src_cursor.start(), level);
            let dst_index = Self::get_index(dst_cursor.start(), level);

            let src_entry = &mut self[src_index];

            if !src_entry.is_present() {
                assert!(!dest[dst_index].is_present());
                *src_cursor = src_cursor.advance_until_empty(Self::level_to_page_size(level));
                *dst_cursor = dst_cursor.advance_until_empty(Self::level_to_page_size(level));
                continue;
            }

            let is_aligned = src_cursor
                .start()
                .is_aligned_to(Self::level_to_page_size(level))
                && dst_cursor
                    .start()
                    .is_aligned_to(Self::level_to_page_size(level))
                && dst_cursor.remaining() >= Self::level_to_page_size(level)
                && src_cursor.remaining() >= Self::level_to_page_size(level);

            if !is_aligned || level > max_level {
                log::log!(
                    LOG_LEVEL,
                    "not aligned for this level: src_cursor {:?}, dst_cursor {:?}, level {}",
                    src_cursor,
                    dst_cursor,
                    level
                );
                if src_entry.is_huge() && level != Self::last_level() {
                    self.split_huge(src_index, level, consist, src_cursor.start(), fa)?;
                }
                assert!(level > 0);
                dest.populate(dst_index, EntryFlags::intermediate(), fa)?;

                let next_dest_table = dest.next_table_mut(dst_index).unwrap();
                let next_src_table = self.next_table_mut(src_index).unwrap();
                log::log!(
                    LOG_LEVEL,
                    "setup_cow_range: descending to level {} for src_index {}, dst_index {}",
                    level - 1,
                    src_index,
                    dst_index
                );
                next_src_table.setup_cow_range(
                    next_dest_table,
                    src_cursor,
                    dst_cursor,
                    level - 1,
                    consist,
                    max_level,
                    fa,
                )?;
                continue;
            }

            let src_flags = src_entry.flags();
            let src_addr = src_entry.addr(level);
            let src_frame = get_frame(src_addr).unwrap();
            src_frame.inc_refcount();
            src_frame.set_cow(true);
            log::trace!(
                "copying entry without write: src_index {}, dst_index {}, level {}, src_addr {:x}, src_flags {:?}",
                src_index,
                dst_index,
                level,
                src_addr,
                src_flags
            );

            self.update_entry(
                consist,
                src_index,
                Entry::new(src_addr, src_flags - EntryFlags::WRITE),
                src_cursor.start(),
                true,
                level,
            );

            dest.update_entry(
                consist,
                dst_index,
                Entry::new(src_addr, src_flags - EntryFlags::WRITE),
                dst_cursor.start(),
                true,
                level,
            );

            *src_cursor = src_cursor.advance_until_empty(Self::level_to_page_size(level));
            *dst_cursor = dst_cursor.advance_until_empty(Self::level_to_page_size(level));
        }

        Ok(())
    }

    /// How many frames a [`Self::map`] of `cursor` starting at this table may have to allocate.
    ///
    /// `MappingCursor::max_number_new_tables` answers the same question from the *geometry* alone
    /// -- it returns one per level regardless of what is already installed, so a 4 KiB mapping
    /// always asks for `top_level` frames. This walks what is actually there instead, and on a
    /// sequential fault run the tables exist already: measured at **one `populate` per 205
    /// `map_page` calls** on `page_fault_zero_fill`.
    ///
    /// **This mirrors [`Self::map`]'s own loop**, entry by entry, rather than following the single
    /// path under `cursor.start()`. An earlier version walked one path, which is exact for the
    /// one-page cursor `map_page` hands it and a silent under-count for anything longer: a range
    /// spanning 512 level-1 entries can need 512 tables and the path walk would have answered
    /// with what the *first* one needed. Under-counting is the failure that matters -- the caller
    /// precharges this, and a short precharge sends `try_allocate` to the global allocator with no
    /// `WAIT_OK` while the object's page-table lock is held. `FA_ALLOC_AVOID_EMPTY` --
    /// `avoid-empty=` on `PERFMARK-FA` -- is the live tripwire for exactly that and must stay 0.
    ///
    /// Exact only in the safe direction elsewhere too: a present huge entry is charged for a split
    /// `map` will not actually perform (it skips huge entries), and an unfollowable entry is
    /// charged as if absent.
    ///
    /// `examined` accumulates entries inspected, which is the cost of asking. It is the thing to
    /// read before extending this to a whole-object cursor: the walk descends only where a table
    /// is present, so an absent subtree costs one entry, but a fully-populated 1 GiB object costs
    /// 1 + 512.
    ///
    /// Correct to read without synchronisation because callers hold the object's page-table lock
    /// across both this and the `map` it predicts for, so nothing can install or remove a table in
    /// between.
    pub(super) fn tables_needed(
        &self,
        cursor: &MappingCursor,
        level: usize,
        examined: &mut usize,
    ) -> usize {
        if level == 0 {
            return 0;
        }
        let start_index = Self::get_index(cursor.start(), level);
        let mut cur = *cursor;
        let mut count = 0;
        for idx in start_index..Self::PAGE_TABLE_ENTRIES {
            if cur.remaining() == 0 {
                break;
            }
            *examined += 1;
            let entry = self[idx];
            let sub = cur.clipped_to_entry(level);
            if !entry.is_present() {
                // Absent: `map` populates here and, below, at every remaining level, for the whole
                // of this entry's share of the range.
                count += sub.max_number_new_tables(level, 0);
            } else if entry.is_huge() && Self::can_map_at_level(level) {
                // Huge and splittable: `split_huge` allocates the table it installs.
                count += 1 + sub.max_number_new_tables(level, 0);
            } else if let Some(next) = self.next_table(idx) {
                // A COW intermediate: `map` calls `do_cow_copy`, which allocates a replacement
                // table. Checked rather than assumed absent -- `setup_cow_range` marks these, so
                // it is reachable whenever an object has been cloned.
                let cow = self.next_table_frame(idx).is_some_and(|f| f.is_cow());
                count +=
                    usize::from(cow) + next.tables_needed(&sub, Self::next_level(level), examined);
            } else {
                // `is_present() && !is_huge()` should always resolve, but a table we cannot follow
                // is one we cannot make a claim about.
                count += sub.max_number_new_tables(level, 0);
            }
            let Some(next) = cur.align_advance(Self::level_to_page_size(level)) else {
                break;
            };
            cur = next;
        }
        count
    }

    /// How many frames a [`Self::cow_copy`] of the path under `cursor.start()` may allocate.
    ///
    /// A different question from [`Self::tables_needed`], and it has to be asked separately:
    /// `cow_copy` allocates from `do_cow_copy` (only for an entry whose frame is marked COW) and
    /// from `split_huge`, never from `populate`, so a predictor written for `map` does not
    /// describe it. It descends one path, so the walk is at most `top_level + 1` entries.
    ///
    /// This is also strictly more than the geometric answer it replaces in the worst case, and
    /// deliberately so. `maybe_cow_at` charged `max_number_new_tables(top_level, 0)` -- 2 on
    /// object tables -- while a path that is COW at every level allocates at level 2 (replacement
    /// intermediate), level 1 (its child) and level 0 (the data frame): **three**. That
    /// under-count has never tripped `avoid-empty` only because the bench that watches it
    /// (`page_fault_zero_fill`) reports `cow` count 0, i.e. the tripwire has no positive control
    /// on this path.
    pub(super) fn cow_tables_needed(
        &self,
        cursor: &MappingCursor,
        level: usize,
        examined: &mut usize,
    ) -> usize {
        let index = Self::get_index(cursor.start(), level);
        *examined += 1;
        let entry = self[index];
        if !entry.is_present() {
            return 0;
        }
        if entry.is_huge() && level != Self::last_level() {
            // `split_huge` allocates the table it installs, and `cow_copy` then descends into it.
            // What that descent finds is a table that did not exist when we looked, so charge the
            // whole remaining chain rather than claiming to know.
            return 1 + level;
        }
        let mut count =
            usize::from(get_frame(entry.addr(level)).is_some_and(|frame| frame.is_cow()));
        if level > 0
            && let Some(next) = self.next_table(index)
        {
            count += next.cow_tables_needed(cursor, Self::next_level(level), examined);
        }
        count
    }

    pub(super) fn map(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        phys: &mut impl PhysAddrProvider,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
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
            let Some(paddr) = phys.peek() else {
                break;
            };
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
                let t_leaf = crate::obj::pagetables::mapprobe::start();
                let t_gf = crate::obj::pagetables::mapprobe::start();
                let known = if PROVIDER_CARRIES_FRAME {
                    paddr.frame
                } else {
                    None
                };
                if let Some(frame) = known.or_else(|| get_frame(paddr.addr))
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
                crate::obj::pagetables::mapprobe::record(
                    &crate::obj::pagetables::mapprobe::W_LEAF_GF_NS,
                    t_gf,
                );
                // `DIRTY` here means "may need writeback", not "was written" -- the kernel writes
                // object data through the direct map, which this entry never sees. A probed
                // mapping opts out so unmap can read the bit for what the hardware put there;
                // only anonymous fills do that, and their dirty list is discarded. Per entry, not
                // per call: a provider covering several pages probes each one it installs.
                let probed =
                    zeroprobe::ENABLED && paddr.settings.flags().contains(MappingFlags::PROBE);
                if probed {
                    zeroprobe::record_install();
                }
                self.update_entry(
                    consist,
                    idx,
                    Entry::new(
                        paddr.addr,
                        EntryFlags::from(&paddr.settings)
                            | if probed {
                                EntryFlags::empty()
                            } else {
                                EntryFlags::DIRTY
                            }
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
                crate::obj::pagetables::mapprobe::record(
                    &crate::obj::pagetables::mapprobe::W_LEAF_NS,
                    t_leaf,
                );
                crate::obj::pagetables::mapprobe::tick(
                    &crate::obj::pagetables::mapprobe::W_LEAF_CALLS,
                );
            } else {
                assert_ne!(level, Self::last_level());
                let t_desc = crate::obj::pagetables::mapprobe::start();
                let t_pop = crate::obj::pagetables::mapprobe::start();
                self.populate(idx, EntryFlags::intermediate(), fa)?;
                crate::obj::pagetables::mapprobe::record(
                    &crate::obj::pagetables::mapprobe::W_POPULATE_NS,
                    t_pop,
                );
                // Split out rather than left inline: the lookup is the measured quantity and the
                // branch is almost never taken, so timing the `if` would time the wrong thing.
                let t_cow = crate::obj::pagetables::mapprobe::start();
                let next_is_cow = self.next_table_frame(idx).is_some_and(|f| f.is_cow());
                crate::obj::pagetables::mapprobe::record(
                    &crate::obj::pagetables::mapprobe::W_COW_GF_NS,
                    t_cow,
                );
                if next_is_cow {
                    self.do_cow_copy(idx, level, consist, cursor.start(), false, fa)?;
                }
                let next_table = self.next_table_mut(idx).unwrap();
                // Recorded before descending: a parent's span must not contain its child's.
                crate::obj::pagetables::mapprobe::record(
                    &crate::obj::pagetables::mapprobe::W_DESCEND_NS,
                    t_desc,
                );
                next_table.map(consist, cursor, Self::next_level(level), phys, fa)?;
            }

            if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
        Ok(())
    }

    pub(super) fn setup_zero_range(
        &mut self,
        consist: &mut Consistency,
        cursor: &mut MappingCursor,
        level: usize,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        // How far the cursor is from the end of the entry it currently sits in.
        // `advance_until_empty` steps by exactly its argument, so handing it the level's page size
        // overshoots into the next entry whenever the cursor starts mid-entry.
        let to_entry_end = |cursor: &MappingCursor| {
            let size = Self::level_to_page_size(level);
            size - (cursor.start().raw() as usize % size)
        };
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
            if cursor.remaining() == 0 {
                break;
            }
            let mut entry = self[idx];
            let mut is_huge = entry.is_huge() && Self::can_map_at_level(level);
            if cursor.remaining() < Self::level_to_page_size(level) && is_huge {
                self.split_huge(idx, level, consist, cursor.start(), fa)?;
                // `split_huge` replaces this slot with an intermediate entry, so the copy taken
                // above no longer describes it. Acting on the stale copy would drop the whole huge
                // region instead of the sub-range asked for, orphan the table `split_huge` just
                // built along with its children, and free the head child under it. Every other
                // caller re-reads or descends via `next_table_mut` for the same reason.
                entry = self[idx];
                is_huge = entry.is_huge() && Self::can_map_at_level(level);
            }
            if entry.is_present() && (is_huge || level == Self::last_level()) {
                let frame = get_frame(entry.addr(level));
                let flags = entry.flags();
                let mut new_entry = Entry::new_unused();
                new_entry.set_flags(new_entry.flags() | EntryFlags::DIRTY);
                self.update_entry(consist, idx, new_entry, cursor.start(), true, level);
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
                    if zeroprobe::ENABLED && flags.contains(EntryFlags::PROBED) {
                        zeroprobe::record(flags.contains(EntryFlags::DIRTY), frame);
                    }
                    consist.free_frame(frame);
                }
                *cursor = cursor.advance_until_empty(to_entry_end(cursor));
            } else if entry.is_present() && level != Self::last_level() {
                if self.next_table_frame(idx).is_some_and(|f| f.is_cow()) {
                    self.do_cow_copy(idx, level, consist, cursor.start(), false, fa)?;
                }
                let next_table = self.next_table_mut(idx).unwrap();
                // The child walks the same cursor and leaves it at the end of its own coverage.
                // Advancing again here would step a second time over an entry nobody visited: a
                // range spanning two level-1 regions zeroed only the first, and the caller got no
                // error -- which is what `zero_range` shipped to userspace as "this range is now
                // zero". Every entry-consuming arm advances for itself instead.
                next_table.setup_zero_range(consist, cursor, Self::next_level(level), fa)?;
            } else {
                *cursor = cursor.advance_until_empty(to_entry_end(cursor));
            }
        }
        Ok(())
    }

    /// Unmap a range. `released` collects the address of an object page table whose reference this
    /// unmap dropped, so callers can tell "we released *this* object's table" from the much weaker
    /// "something was unmapped" -- the entry at an address may belong to a different object than
    /// the caller expects. Only the last one is kept, which is all a single-region unmap can
    /// produce; callers unmapping wider ranges (context teardown) ignore it.
    pub(super) fn unmap(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        fa: &mut FrameAllocator,
        released: &mut Option<PhysAddr>,
    ) -> Result<bool, TwzError> {
        let start_index = Self::get_index(cursor.start(), level);
        let mut did_unmap = false;
        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];
            let is_huge = entry.is_huge() && Self::can_map_at_level(level);
            if entry.is_present() && (is_huge || level == Self::last_level()) {
                let frame = get_frame(entry.addr(level));
                let flags = entry.flags();
                did_unmap = true;
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
                    if zeroprobe::ENABLED && flags.contains(EntryFlags::PROBED) {
                        zeroprobe::record(flags.contains(EntryFlags::DIRTY), frame);
                    }
                    consist.free_frame(frame);
                }
            } else if entry.is_present() && level != Self::last_level() {
                if !entry.is_object_table() {
                    if self.next_table_frame(idx).is_some_and(|f| f.is_cow()) {
                        self.do_cow_copy(idx, level, consist, cursor.start(), false, fa)?;
                    }
                    let next_table = self.next_table_mut(idx).unwrap();
                    did_unmap |=
                        next_table.unmap(consist, cursor, Self::next_level(level), fa, released)?;
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
                        did_unmap = true;
                    }
                } else {
                    did_unmap = true;
                    *released = Some(entry.table_addr());
                    get_frame(entry.table_addr()).unwrap().dec_refcount();
                    // Positive-control window: suppresses ONLY this detach's invalidation, and
                    // only when `posctl::UNMAP_NO_INVL` is armed (ships OFF, compiles out).
                    consist.set_suppress(true);
                    self.update_entry(
                        consist,
                        idx,
                        Entry::new_unused(),
                        cursor.start(),
                        false,
                        level,
                    );
                    // Detaching an object-table link orphans every TLB entry cached through the
                    // subtree, and `update_entry`'s single non-terminal invlpg is only
                    // architecturally required to cover one page plus the paging-structure
                    // caches — leaf entries elsewhere in the slot survive per spec. Measured:
                    // suppressing that invlpg read stale on 5120/5120 probes
                    // (`tlb_stale_slot_reuse`, flushctl arm), i.e. current safety rested on the
                    // implementation over-invalidating. Escalate to a full non-global flush for
                    // this target; remote cpus stay covered by the PCID revoke in `finish_send`.
                    consist.set_full();
                    consist.set_suppress(false);
                }
            }

            if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
        Ok(did_unmap)
    }

    pub(super) fn change(
        &mut self,
        consist: &mut Consistency,
        mut cursor: MappingCursor,
        level: usize,
        settings: &MappingSettings,
        fa: &mut FrameAllocator,
    ) -> Result<(), TwzError> {
        let start_index = Self::get_index(cursor.start(), level);
        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
            let entry = &mut self[idx];
            let is_present = entry.is_present();
            let is_huge = entry.is_huge() && Self::can_map_at_level(level);
            let addr = entry.addr(level);

            if is_present && (is_huge || level == Self::last_level()) {
                // If this is a COW page, perform the copy before changing permissions.
                // entry borrow ends here (last use of entry is addr above).
                if let Some(frame) = get_frame(addr)
                    && frame.is_cow()
                {
                    self.do_cow_copy(idx, level, consist, cursor.start(), false, fa)?;
                }
                // Re-read addr after potential COW copy.
                let new_addr = self[idx].addr(level);
                self.update_entry(
                    consist,
                    idx,
                    Entry::new(
                        new_addr,
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
                    self.do_cow_copy(idx, level, consist, cursor.start(), false, fa)?;
                }
                let next_table = self.next_table_mut(idx).unwrap();
                next_table.change(consist, cursor, Self::next_level(level), settings, fa)?;
            }

            if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
        Ok(())
    }

    pub fn with_dirty_bits(
        &mut self,
        mut cursor: MappingCursor,
        level: usize,
        consist: &mut Consistency,
        cb: &mut impl FnMut(MapInfo) -> bool,
    ) -> Result<bool, TwzError> {
        let start_index = Self::get_index(cursor.start(), level);
        let mut did_clear = false;
        for idx in start_index..Table::PAGE_TABLE_ENTRIES {
            let entry = self[idx];
            let is_present = entry.is_present();
            let is_huge = entry.is_huge() && Self::can_map_at_level(level);
            let is_dirty = entry.flags().contains(EntryFlags::DIRTY);
            if is_present && (is_huge || level == Self::last_level()) {
                if is_dirty {
                    let info = MapInfo::new(
                        cursor.start(),
                        entry.addr(level),
                        entry.flags().settings(),
                        Self::level_to_page_size(level),
                    );
                    let clear = cb(info);
                    if clear {
                        self.update_entry(
                            consist,
                            idx,
                            Entry::new(entry.addr(level), entry.flags() - EntryFlags::DIRTY),
                            cursor.start(),
                            true,
                            level,
                        );
                        did_clear |= true;
                    }
                }
            } else if is_present && level != Self::last_level() {
                let next_table = self.next_table_mut(idx).unwrap();
                did_clear |=
                    next_table.with_dirty_bits(cursor, Self::next_level(level), consist, cb)?;
            }

            if let Some(next) = cursor.align_advance(Self::level_to_page_size(level)) {
                cursor = next;
            } else {
                break;
            }
        }
        Ok(did_clear)
    }

    pub(super) fn is_object_mapped(
        &self,
        cursor: MappingCursor,
        level: usize,
        settings: MappingSettings,
    ) -> bool {
        let index = Self::get_index(cursor.start(), level);
        let entry = &self[index];
        if entry.is_present() && entry.is_object_table() {
            // TODO: check cache type compatible.
            if entry.flags().perms() & settings.perms() != settings.perms() {
                return false;
            }
            return true;
        } else if entry.is_present() && level != Self::last_level() {
            let next_table = self.next_table(index).unwrap();
            return next_table.is_object_mapped(cursor, Self::next_level(level), settings);
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
            // NOTE (spawnbench.md §21): reporting one entry's worth here is correct but slow --
            // `do_read_map` restarts at the root for every step, so `count_pages` over an object's
            // 1 GiB `max_len()` spends 37 us to find 16 pages. Scanning forward within the table
            // for the next present entry and returning the whole absent run measured **16.5x**
            // faster (38,540 -> 2,338 ns) with an identical page count.
            //
            // Not shipped, and the reason is now understood: the arm carrying it went 3/5 FAILED
            // with "out of pager request slots" -- since diagnosed as an independent deadlock in
            // the pager-memory donation path that any stat speedup unearths (`pagerwedge.md`),
            // and fixed there. The scan itself was never implicated, but it is also superseded:
            // `COUNT_PAGES_COUNTER` answers `count_pages` without walking at all (213x), so this
            // path only matters for `readmap`-style callers that still iterate sparse ranges.
            Err(Table::level_to_page_size(level))
        }
    }

    pub(super) fn is_empty_at_level(
        &self,
        cursor: &MappingCursor,
        target_level: usize,
        level: usize,
    ) -> bool {
        if self.read_count() == 0 {
            return true;
        }

        let index = Self::get_index(cursor.start(), level);

        if !self[index].is_present() {
            return true;
        }

        if level == target_level {
            return false;
        }

        if let Some(next_table) = self.next_table(index) {
            return next_table.is_empty_at_level(cursor, target_level, Self::next_level(level));
        } else {
            false
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

/// Does the non-leaf arm of [Table::do_cow_copy] ever execute, and over how many entries?
///
/// That arm clears `WRITE` on every present entry of a whole sub-table and then enqueues a single
/// `invlpg` for the caller's address, so it downgrades up to 512 pages and invalidates one. Before
/// fixing that, establish whether anything reaches it: a fix for an invalidation that nothing
/// exercises cannot be demonstrated to work. See TLB.md.
pub mod nonleaf_cow {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static ENTRIES: AtomicUsize = AtomicUsize::new(0);
    static MAX_ENTRIES: AtomicUsize = AtomicUsize::new(0);

    pub fn record(downgraded: usize) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        ENTRIES.fetch_add(downgraded, Ordering::Relaxed);
        MAX_ENTRIES.fetch_max(downgraded, Ordering::Relaxed);
    }

    pub fn calls() -> usize {
        CALLS.load(Ordering::Relaxed)
    }

    pub fn entries() -> usize {
        ENTRIES.load(Ordering::Relaxed)
    }

    pub fn print() {
        let calls = CALLS.load(Ordering::Relaxed);
        emerglogln!(
            "== nonleaf cow: {} calls, {} entries downgraded, {} max per call ({} invalidated)",
            calls,
            ENTRIES.load(Ordering::Relaxed),
            MAX_ENTRIES.load(Ordering::Relaxed),
            calls,
        );
    }
}
