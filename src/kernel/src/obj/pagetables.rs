use alloc::vec::Vec;

use itertools::Itertools;
use twizzler_abi::device::CacheType;
use twizzler_rt_abi::error::TwzError;

use crate::{
    arch::{
        PhysAddr, VirtAddr,
        context::ArchContextTarget,
        memory::pagetables::{ArchTlbMgr, PendingShootdown},
    },
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, get_frame, min_level_for_len},
        pagetables::{
            Consistency, ContiguousProvider, DeferredUnmappingOps, MapInfo, MapReader, Mapper,
            MappingCursor, MappingSettings, Table, TlbOrigin,
        },
        tracker::{
            FrameAllocFlags, FrameAllocator, alloc_frame, free_frame, take_or_new_frame_allocator,
        },
    },
    obj::{Object, ObjectRef, PageNumber},
};

const MAX_INVL_TARGETS: usize = 8;
const MAX_INVLS: usize = 4;

/// Second-and-later operations parked under one page-table lock hold, merged into the batch the
/// guard will discharge. See [ObjectPageTable::park].
///
/// Measured at ~150k per boot, which is why `park` merges rather than discharging the older batch
/// inline: a lock hold runs one consistency-generating operation *per page* of a page-in or copy
/// loop, not one per hold. Under the discharge-inline design only the last park in a hold survived
/// to the guard, so on the order of (k-1)/k of all real waits stayed inside the lock -- the thing
/// the guard exists to prevent. Kept as a counter because it is the number that falsified the
/// "one operation per hold" assumption, and would catch a future change that reintroduced it.
pub mod merged_parks {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    pub fn record() {
        N.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count() -> usize {
        N.load(Ordering::Relaxed)
    }
}

/// What became of a run of pages the pager delivered.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct InstallTally {
    pub installed: usize,
    /// Pages the object already held, so the delivery was wasted.
    pub dup: usize,
    /// Of `dup`, those a large page already covers. Worth counting apart from the small case: one
    /// overlapping request that loses the race to a merge has every one of its pages in that
    /// 2 MiB region rejected, so a handful of merges can account for a great many duplicates.
    pub dup_large: usize,
}

pub struct ObjectPageTable {
    mapper: Mapper,
    invls: heapless::Vec<
        (ArchContextTarget, heapless::Vec<MappingCursor, MAX_INVLS>),
        MAX_INVL_TARGETS,
    >,
    map_count: usize,
    /// Work that must happen after this object's page-table lock is released: waiting for the
    /// shootdowns issued under it, and then freeing the frames those shootdowns protect.
    ///
    /// Parked here rather than run inline because the wait dominates the lock hold -- a median
    /// 90 ms per boot of object-origin wait time, all of it with this mutex held (see TLB.md) --
    /// and none of it needs the lock. [PtGuard] takes it and runs it after unlocking; living behind
    /// the same mutex as everything else here is what makes that handoff safe.
    deferred: Option<DeferredUnmappingOps>,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct FindFrameFlags: u32 {
        const ALLOW_NOT_ZEROED = (1 << 0);
        const WRITE = (1 << 1);
        const POPULATE = (1 << 2);
    }
}

impl Drop for ObjectPageTable {
    fn drop(&mut self) {
        let mut consist = Consistency::new_object_tables();
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), self.max_len());
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let _ = self.mapper.unmap(cursor, &mut consist, &mut fa, &mut None);
        self.run_consistency(consist);
        // No guard is going to come along and discharge this -- the object is going away -- so the
        // parked work has to run here, before the root frame below is freed.
        if let Some(ops) = self.deferred.take() {
            ops.run_all();
        }
        let root_frame = get_frame(self.mapper.root_address()).expect("root frame should exist");
        root_frame.set_pt(false);
        if root_frame.dec_refcount() == 0 {
            free_frame(root_frame);
        }
    }
}

#[derive(Default, Debug)]
pub struct DirtyList {
    pages: Vec<(PageNumber, PhysAddr, usize)>,
    frames: Vec<FrameRef>,
}

impl DirtyList {
    pub fn pages(&self) -> &Vec<(PageNumber, PhysAddr, usize)> {
        &self.pages
    }

    pub fn frames(&self) -> &Vec<FrameRef> {
        &self.frames
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

impl Drop for DirtyList {
    fn drop(&mut self) {
        for frame in self.frames.drain(..) {
            if frame.dec_refcount() == 0 {
                free_frame(frame);
            }
        }
    }
}

impl ObjectPageTable {
    pub fn new() -> Self {
        let frame = alloc_frame(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
        );
        frame.set_pt(true);
        frame.inc_refcount();
        let mut mapper = Mapper::new(frame.start_address());
        mapper.set_start_level(Self::top_level());
        Self {
            mapper,
            invls: heapless::Vec::new(),
            map_count: 0,
            deferred: None,
        }
    }

    /// Hand post-unlock work to [PtGuard].
    ///
    /// Two operations under one lock hold is rare, so rather than merging frame lists the older one
    /// is discharged inline here -- which is exactly the behaviour every one of these call sites had
    /// before parking existed.
    ///
    /// But that inline discharge runs the shootdown wait and the frame frees back *inside* the hold
    /// this whole change exists to shorten, so the improvement is conditional on there being one
    /// consistency-generating operation per lock hold. Counted rather than assumed: zero per boot
    /// proves the fast path, and anything else names the site, instead of showing up later as an
    /// unexplained number in the hold-time instrumentation.
    fn park(&mut self, ops: DeferredUnmappingOps) {
        match self.deferred.as_mut() {
            Some(prev) => {
                prev.absorb(ops);
                merged_parks::record();
            }
            None => self.deferred = Some(ops),
        }
    }

    pub fn take_deferred(&mut self) -> Option<DeferredUnmappingOps> {
        self.deferred.take()
    }

    pub fn top_level() -> usize {
        Table::top_level() - 1
    }

    pub fn map_count(&self) -> usize {
        self.map_count
    }

    /// The table a context's page tables point at for this object, i.e. what [Mapper::object_map]
    /// installs and what unmapping releases. `None` if this object has never been mapped. Used to
    /// tell an unmap of *our* mapping from an unmap of whatever else ended up at that address.
    pub fn context_table_addr(&self) -> Option<PhysAddr> {
        self.mapper.peek_table_addr(Self::top_level() - 1)
    }

    pub fn inc_map_count(&mut self) {
        self.map_count += 1;
    }

    pub fn dec_map_count(&mut self) {
        assert!(self.map_count > 0, "map count cannot be negative");
        self.map_count -= 1;
    }

    pub fn add_invalidate(&mut self, target: ArchContextTarget, cursor: MappingCursor) {
        if let Some((_, maps)) = self.invls.iter_mut().find(|(t, _)| *t == target) {
            if !maps.iter().contains(&cursor) {
                let _ = maps.push(cursor);
            }
        } else {
            let mut maps = heapless::Vec::new();
            let _ = maps.push(cursor);
            let _ = self.invls.push((target, maps));
        }
    }

    pub fn remove_invalidate(&mut self, target: ArchContextTarget, cursor: MappingCursor) {
        if self.invls.is_full() || self.invls.iter().any(|(_, maps)| maps.is_full()) {
            // We might have hit the limit.
            return;
        }
        if let Some((_, maps)) = self.invls.iter_mut().find(|(t, _)| *t == target) {
            if let Some(pos) = maps.iter().position(|c| c.start() == cursor.start()) {
                maps.swap_remove(pos);
            }
        }
    }

    pub fn max_len(&self) -> usize {
        PHYS_LEVEL_LAYOUTS[self.mapper.start_level()].size()
    }

    pub fn invalidate(&mut self, offset: u64, len: usize) {
        log::trace!(
            "invalidating offset {:x} len {:x} (max len {:x}) {} {} {}",
            offset,
            len,
            self.max_len(),
            self.invls.is_empty(),
            self.invls.is_full(),
            self.invls.iter().any(|(_, maps)| maps.is_full()),
        );
        if self.invls.is_empty() {
            return;
        }
        if self.invls.is_full() || self.invls.iter().any(|(_, maps)| maps.is_full()) {
            let mut tlb = ArchTlbMgr::new_full_global();
            tlb.set_origin(TlbOrigin::Object);
            tlb.finish();
            return;
        }
        // Same shape as send_consistency: send for every context, then wait once, rather than a
        // complete IPI-and-wait round per context with the object's page-table lock held.
        let mut pending = PendingShootdown::none();
        for (target, maps) in self.invls.iter() {
            if maps.is_empty() {
                continue;
            }

            let mut tlb = ArchTlbMgr::new(*target);
            tlb.set_origin(TlbOrigin::Object);
            for map in maps.iter() {
                if map.start().is_kernel() {
                    // The kernel half's page tables are shared by every context, so these
                    // translations can be cached under any PCID -- and a targeted invlpg reaches
                    // only the executing cpu's current one. Nothing short of the PGE toggle behind
                    // a full+global batch covers them, which is what ArchContext::lock_with_consist
                    // already does for every other kernel-range mapping change.
                    tlb.set_full_global();
                    continue;
                }
                let mut len = map.remaining().min(len);
                let addr = match map.start().offset(offset as usize) {
                    Ok(addr) => addr,
                    Err(_) => {
                        len = self.max_len();
                        map.start()
                    }
                };
                tlb.enqueue(
                    addr,
                    false,
                    true,
                    min_level_for_len(len).unwrap_or(self.mapper.start_level()),
                );
            }
            pending.absorb(tlb.finish_send());
        }
        self.park(DeferredUnmappingOps::from_pending(pending));
    }

    pub fn invalidate_page(&mut self, pn: PageNumber) {
        let offset = pn.as_byte_offset() as u64;
        let len = PageNumber::PAGE_SIZE;
        self.invalidate(offset, len);
    }

    pub fn invalidate_full(&mut self) {
        self.invalidate(0, self.max_len());
    }

    pub fn run_consistency2(&mut self, mut consist: Consistency, other: &Self) {
        // Both objects get sent to. They did not before: `do_run_consistency` reset the accumulated
        // invalidations at the end, so the second call always found nothing pending and `other`'s
        // contexts were never invalidated at all -- visible in the old code as a `(None, Some(_))`
        // arm that could not be reached. The reset now happens once, after both.
        let mut pending = self.send_consistency(&mut consist);
        pending.absorb(other.send_consistency(&mut consist));
        consist.tlb_mut().reset();
        consist.set_pending(pending);
        let ops = consist.into_deferred();
        self.park(ops);
    }

    pub fn run_consistency(&mut self, mut consist: Consistency) {
        let pending = self.send_consistency(&mut consist);
        consist.tlb_mut().reset();
        consist.set_pending(pending);
        let ops = consist.into_deferred();
        self.park(ops);
    }

    /// Send the accumulated invalidations to every context this object is mapped into -- one
    /// shootdown per context -- and return their combined obligation, unwaited.
    ///
    /// One per context rather than one merged across all of them, because `ArchTlbMgr::merge` on
    /// two different `target_cr3`s has no precise common representation and degrades to full *and*
    /// global. Global is the expensive word: `should_target` returns true for every processor when
    /// it is set, which defeats the PCID revocation that normally reduces the target set to zero or
    /// one, and every receiver then does a CR4.PGE toggle and a full flush. Measured at ~2200 of
    /// those per boot against the arch mapper's ~320 (see TLB.md). Sending all of them before
    /// waiting for any is what keeps the precise version from costing N serial rounds instead.
    fn send_consistency(&self, consist: &mut Consistency) -> PendingShootdown {
        if !consist.tlb().has_pending() {
            return PendingShootdown::none();
        }
        // `add_invalidate` drops silently once its bounded lists fill, so past MAX_INVL_TARGETS
        // contexts (or MAX_INVLS cursors within one) this object no longer knows where all of its
        // mappings live. Retargeting precisely would then reach only the contexts that happened to
        // fit and skip the rest entirely -- the same reason `invalidate` gives up and goes global.
        let overflowed = self.invls.is_full() || self.invls.iter().any(|(_, maps)| maps.is_full());
        if consist.tlb().is_full() || overflowed {
            let mut tlb = ArchTlbMgr::new_full_global();
            tlb.set_origin(TlbOrigin::Object);
            return tlb.finish_send();
        }

        let mut pending = PendingShootdown::none();
        for (target, maps) in self.invls.iter() {
            if maps.is_empty() {
                continue;
            }

            consist.tlb_mut().set_target(*target);

            // Merging within one target stays precise -- same `target_cr3` -- so the per-context
            // send still covers all of that context's cursors in one round.
            let mut per_target: Option<ArchTlbMgr> = None;
            for map in maps.iter() {
                let mut tlb = consist.tlb().apply_offset_from_map(map);
                // See Self::invalidate: a kernel-range mapping is visible under every PCID, so
                // precise invalidation cannot reach all of its copies. Now this makes only its own
                // context's send global rather than poisoning every other context's too.
                if map.start().is_kernel() {
                    tlb.set_full_global();
                }

                match per_target {
                    Some(ref mut acc) => acc.merge(tlb),
                    None => per_target = Some(tlb),
                }
            }
            if let Some(mut tlb) = per_target {
                pending.absorb(tlb.finish_send());
            }
        }
        pending
    }

    pub fn map_page(&mut self, offset: u64, page: FrameRef) -> Result<(), TwzError> {
        let mut consist = Consistency::new_object_tables();
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), page.size());
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Self::top_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        let mut phys = ContiguousProvider::new(
            page.start_address(),
            page.size(),
            MappingSettings::default_user(),
        );
        let r = self.mapper.map(cursor, &mut phys, &mut consist, &mut fa);
        self.run_consistency(consist);
        r
    }

    /// Install a contiguous run of 4 KiB frames in one descent of the page tables.
    ///
    /// [`Self::map_page`] costs a walk from the root, a frame-allocator precharge and an
    /// invalidation pass *per page*, and the pager delivers ~130 pages per completion; this pays
    /// each of those once for the run. `Table::map` skips entries that are already present, taking
    /// no reference on their frames, so a run can be mapped whole without first being split around
    /// the pages the object turns out to hold.
    pub fn map_pages(
        &mut self,
        offset: u64,
        start: PhysAddr,
        npages: usize,
    ) -> Result<(), TwzError> {
        if npages == 0 {
            return Ok(());
        }
        let len = npages * PageNumber::PAGE_SIZE;
        let mut consist = Consistency::new_object_tables();
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), len);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Self::top_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        // Page-sized offers, not the whole run: these are separate frames, and a huge entry over
        // them would hold one refcount over memory owned by 512 of them. See
        // [`ContiguousProvider::new_of_page_size`].
        let mut phys = ContiguousProvider::new_of_page_size(
            start,
            len,
            PageNumber::PAGE_SIZE,
            MappingSettings::default_user(),
        );
        let r = self.mapper.map(cursor, &mut phys, &mut consist, &mut fa);
        self.run_consistency(consist);
        r
    }

    /// Which pages of a run the object already holds, and whether to a large entry.
    ///
    /// One descent, where asking per page was two of them each -- `is_empty_at_level` and then a
    /// `readmap` to tell a 4 KiB entry from the large page covering it. The reader yields only
    /// present entries, so what it does not report is what [`Self::map_pages`] will install.
    fn tally_present(&mut self, offset: u64, npages: usize) -> InstallTally {
        let len = npages * PageNumber::PAGE_SIZE;
        let end = offset + len as u64;
        let mut tally = InstallTally::default();
        for info in self.readmap(offset, len) {
            // A large entry reports its own aligned base and length, either of which can reach
            // outside the run, so count only the overlap.
            let lo = info.vaddr().raw().max(offset);
            let hi = (info.vaddr().raw().saturating_add(info.len() as u64)).min(end);
            let pages = hi.saturating_sub(lo) as usize / PageNumber::PAGE_SIZE;
            tally.dup += pages;
            if info.len() > PageNumber::PAGE_SIZE {
                tally.dup_large += pages;
            }
        }
        tally.dup = tally.dup.min(npages);
        tally.dup_large = tally.dup_large.min(tally.dup);
        tally.installed = npages - tally.dup;
        tally
    }

    pub fn readmap(&'_ mut self, offset: u64, len: usize) -> MapReader<'_> {
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), len);
        self.mapper.readmap(cursor)
    }

    pub fn with_mapper<R>(&mut self, f: impl FnOnce(&mut Mapper) -> R) -> R {
        f(&mut self.mapper)
    }

    pub fn print_tree(&self) {
        self.mapper.print_tables();
    }

    pub fn count_pages(&self) -> usize {
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), self.max_len());
        let reader = self.mapper.readmap(cursor).coalesce();
        reader.fold(0, |acc, mi| {
            if mi.is_empty() {
                acc
            } else {
                acc + mi.len() / PageNumber::PAGE_SIZE
            }
        })
    }

    /// Bucket every populated 2 MiB region of this object into `out`.
    ///
    /// One pass of the raw (uncoalesced) map reader: entries arrive in address order and never
    /// straddle a region, so grouping is a comparison against the running region base.
    pub fn promotion_census(&self, out: &mut PromotionCensus) {
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), self.max_len());
        let mut acc: Option<RegionAcc> = None;
        for mi in self.mapper.readmap(cursor) {
            if mi.is_empty() {
                continue;
            }
            let base = mi.vaddr().raw() & !(PHYS_LEVEL_LAYOUTS[1].size() as u64 - 1);
            if acc.as_ref().is_some_and(|acc| acc.base != base) {
                acc.take().unwrap().record(out);
            }
            acc.get_or_insert_with(|| RegionAcc::new(base)).add(&mi);
        }
        if let Some(acc) = acc {
            acc.record(out);
            out.objects += 1;
        }
    }

    pub fn get_dirty_and_reset(&mut self) -> Result<DirtyList, TwzError> {
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), self.max_len());

        fn add_to_list(dirty_list: &mut DirtyList, mi: &MapInfo) {
            fn can_append(mi: &MapInfo, item: &(PageNumber, PhysAddr, usize)) -> bool {
                if mi.is_empty() {
                    return false;
                }
                let pn = PageNumber::from_address(mi.vaddr());
                item.0.offset(item.2) == pn
                    && item
                        .1
                        .offset(item.2 * PageNumber::PAGE_SIZE)
                        .is_ok_and(|x| x == mi.paddr())
            }

            let frame = get_frame(mi.paddr()).expect("frame should exist");
            assert!(frame.size() == mi.len());
            dirty_list.frames.push(frame);
            frame.inc_refcount();

            if let Some(pos) = dirty_list
                .pages
                .iter()
                .position(|item| can_append(mi, item))
            {
                dirty_list.pages[pos].2 += mi.len() / PageNumber::PAGE_SIZE;
            } else {
                dirty_list.pages.push((
                    PageNumber::from_address(mi.vaddr()),
                    mi.paddr(),
                    mi.len() / PageNumber::PAGE_SIZE,
                ));
            }
        }

        let mut consist = Consistency::new_object_tables();
        let mut dirty_list = DirtyList::default();
        let r = self.mapper.with_dirty_bits(
            cursor,
            |mi| {
                add_to_list(&mut dirty_list, &mi);
                true
            },
            &mut consist,
        );

        dirty_list.pages.sort_unstable_by_key(|x| x.0);

        self.run_consistency(consist);

        r?;
        Ok(dirty_list)
    }

    pub fn maybe_cow_at(&mut self, offset: u64, mark_dirty: bool) -> Result<bool, TwzError> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Self::top_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );

        let mut consist = Consistency::new_object_tables();
        let did_cow = self
            .mapper
            .cow_at(cursor, &mut consist, mark_dirty, &mut fa);

        self.run_consistency(consist);

        did_cow
    }

    pub fn with_frame<R>(
        &mut self,
        offset: u64,
        flags: FindFrameFlags,
        did_cow: &mut bool,
        f: impl FnOnce(usize, Option<FrameRef>) -> R,
    ) -> Result<R, TwzError> {
        *did_cow = false;
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        if flags.contains(FindFrameFlags::WRITE) {
            *did_cow = self.maybe_cow_at(offset, true)?;
        }
        let mut reader = self.mapper.readmap(cursor);
        let mut page_aligned_offset = offset & !(PHYS_LEVEL_LAYOUTS[0].size() as u64 - 1);
        let mut map_info = reader.next();
        if let Some(mi) = &map_info {
            page_aligned_offset = offset & !(mi.len() as u64 - 1);
        }
        if map_info
            .as_ref()
            .is_some_and(|mi| mi.vaddr().raw() != offset & !(mi.len() as u64 - 1))
        {
            map_info = None;
        }
        let frame_offset = map_info
            .as_ref()
            .map_or(page_aligned_offset as usize, |mi| mi.vaddr().raw() as usize);
        Ok(f(
            frame_offset,
            map_info.and_then(|mi| get_frame(mi.paddr())),
        ))
    }

    /// Get the frame at a given offset. Does not mark the frame dirty.
    pub fn get_frame(&mut self, offset: u64) -> Option<FrameRef> {
        let map_info = self.get_mapinfo(offset)?;
        get_frame(map_info.paddr())
    }

    pub fn get_mapinfo(&mut self, offset: u64) -> Option<MapInfo> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        let mut reader = self.mapper.readmap(cursor);
        reader
            .next()
            .filter(|x| x.vaddr().raw() == offset & !(x.len() as u64 - 1))
    }

    pub fn is_empty_at_level(&mut self, offset: u64, level: usize) -> bool {
        let cursor = MappingCursor::new(
            VirtAddr::new(offset).unwrap(),
            PHYS_LEVEL_LAYOUTS[level].size(),
        );
        self.mapper.is_empty_at_level(&cursor, level)
    }

    pub fn split_to_level(&mut self, offset: u64, level: usize) -> Result<(), TwzError> {
        let mut consist = Consistency::new_object_tables();
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(Self::top_level(), FrameAllocFlags::WAIT_OK);
        let r = self.mapper.split_to_level(
            VirtAddr::new(offset).unwrap(),
            level,
            &mut consist,
            &mut fa,
        );
        self.run_consistency(consist);
        r
    }

    pub fn setup_cow_range(
        &mut self,
        dest: &mut Self,
        src_offset: u64,
        dst_offset: u64,
        len: usize,
    ) -> Result<(), TwzError> {
        let src_cursor = MappingCursor::new(VirtAddr::new(src_offset).unwrap(), len);
        let dst_cursor = MappingCursor::new(VirtAddr::new(dst_offset).unwrap(), len);
        let mut consist = Consistency::new_object_tables();
        let total = src_cursor.max_number_new_tables(Self::top_level(), 0)
            + dst_cursor.max_number_new_tables(Self::top_level(), 0);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(total, FrameAllocFlags::WAIT_OK);
        self.mapper.setup_cow_range(
            &mut dest.mapper,
            src_cursor,
            dst_cursor,
            &mut consist,
            &mut fa,
        )?;
        self.run_consistency2(consist, dest);
        Ok(())
    }

    pub fn setup_zero_range(&mut self, offset: u64, len: usize) -> Result<(), TwzError> {
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), len);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Self::top_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        let mut consist = Consistency::new_object_tables();
        let ops = self.mapper.setup_zero_range(cursor, &mut consist, &mut fa);
        self.run_consistency(consist);
        ops
    }
}

impl Object {
    pub fn map_phys(
        &self,
        offset: usize,
        start: PhysAddr,
        end: PhysAddr,
        ct: CacheType,
    ) -> Result<(), TwzError> {
        let mut pt = self.lock_page_tables();
        let len = (end.raw() - start.raw()) as usize;
        let cursor = MappingCursor::new(VirtAddr::new(offset as u64).unwrap(), len);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(pt.mapper.start_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        let mut phys =
            ContiguousProvider::new(start, len, MappingSettings::default_user().with_cache(ct));
        let mut consist = Consistency::new_object_tables();
        let r = pt.mapper.map(cursor, &mut phys, &mut consist, &mut fa);
        pt.run_consistency(consist);
        r
    }

    pub fn add_frame(&self, pn: PageNumber, frame: FrameRef) {
        let mut pt = self.lock_page_tables();
        pt.map_page(pn.as_byte_offset() as u64, frame).unwrap();
    }

    /// Install `npages` contiguous frames starting at `start` at object page `pn`, skipping any
    /// page the object already holds. Reports how the run was disposed of.
    ///
    /// The pager can deliver a page the object acquired between the request being issued and the
    /// completion landing; two overlapping in-flight requests produce those by construction, since
    /// `add_request` coalesces only on an exact range. `Table::map` already declines to overwrite a
    /// present entry -- and takes no reference on its frame, so the caller's release still frees it
    /// -- which is what lets the whole run go down in one call rather than being split around them.
    ///
    /// Taken as a run rather than per page because everything here is charged per *call*, not per
    /// page: one lock acquisition, one presence pass, one walk from the root, one precharge, one
    /// invalidation. At ~130 pages a completion that is the difference between 130 TLB shootdown
    /// rounds and one.
    pub fn add_frames_if_absent(
        &self,
        pn: PageNumber,
        start: PhysAddr,
        npages: usize,
    ) -> InstallTally {
        let mut pt = self.lock_page_tables();
        let offset = pn.as_byte_offset() as u64;
        let tally = pt.tally_present(offset, npages);
        pt.map_pages(offset, start, npages).unwrap();
        tally
    }

    pub fn cow_clone_page_tables(self: &ObjectRef) -> Result<ObjectPageTable, TwzError> {
        let mut new_pt = ObjectPageTable::new();
        let mut old_pt = self.lock_page_tables();
        assert_eq!(old_pt.mapper.start_level(), new_pt.mapper.start_level());
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), old_pt.max_len());
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(old_pt.mapper.start_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        let mut consist = Consistency::new_object_tables();
        if self.use_pager() {
            old_pt = self.ensure_in_core(
                old_pt,
                PageNumber::from(0),
                cursor.remaining() / PageNumber::PAGE_SIZE,
                &mut false,
                &mut false,
            )?;
        }
        let r = old_pt.mapper.setup_cow_range(
            &mut new_pt.mapper,
            cursor,
            cursor,
            &mut consist,
            &mut fa,
        );
        old_pt.run_consistency(consist);
        r.map(|_| new_pt)
    }
}

/// What large-page *promotion* -- merging a fully-populated 2 MiB region of 4 KiB frames in place
/// -- would find across the object system.
///
/// A large page today is a property of delivery, not of state: it exists only where 512 aligned,
/// contiguous pages arrive in a single pager completion, and a region filled 4 KiB at a time stays
/// 4 KiB forever however contiguous it turns out to be (`largepager.md`). `promotable` is what a
/// promotion pass would convert and is the number that decides whether promotion is worth building.
/// `unaligned` is what it could not convert, and so sizes the pager-side object-keyed allocation
/// that would make promotion always possible.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct PromotionCensus {
    /// Objects with at least one populated region.
    pub objects: usize,
    /// Regions already mapped as one large page.
    pub large: usize,
    /// Full at 4 KiB, physically contiguous, 2 MiB-aligned, and made of frames a merge could
    /// actually take: singly-referenced, not COW, not wired, not page tables.
    pub promotable: usize,
    /// Contiguous and aligned, but the frames are shared -- refcount above one, or COW, or wired.
    /// `merge_frame`'s callers assert against exactly these, and a COW clone of an already-large
    /// region lands here, so counting it as the prize would inflate it by the number of clones.
    pub shared: usize,
    /// Full at 4 KiB, but fragmented or misaligned.
    pub unaligned: usize,
    /// Populated but not full, and the pages in them.
    pub partial: usize,
    pub partial_pages: usize,
    /// Populated region 0s, counted apart: page 0 is the null page and is never mapped, so region
    /// 0 can never be full and would only inflate `partial`.
    pub region0: usize,
    /// Pages in `region0` and `partial` regions -- the two buckets whose page count is not implied
    /// by their region count.
    pub loose_pages: usize,
}

impl PromotionCensus {
    /// Every 4 KiB page the census saw. The region counts claim memory, and the machine has to
    /// actually have it -- comparing this against the allocator is what makes them checkable.
    pub fn pages(&self) -> usize {
        let per_region = PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE;
        (self.large + self.promotable + self.shared + self.unaligned) * per_region
            + self.loose_pages
    }
}

/// One region's worth of accumulation for [ObjectPageTable::promotion_census].
struct RegionAcc {
    base: u64,
    bytes: usize,
    large: bool,
    /// The physical address this region would start at, as implied by an entry's offset within it.
    /// One value agreed by every entry is exactly what "physically contiguous" means here.
    phys_base: Option<u64>,
    contig: bool,
    /// Every frame is one a merge could take. Checked only while `contig` still holds, since a
    /// region that has already lost contiguity cannot be promoted whatever its frames look like --
    /// which keeps the frame lookups off the common fragmented case.
    ///
    /// Mapping settings are not compared: object page tables map with `default_user()` throughout,
    /// and the one thing that varies them is COW, which this already rejects.
    frames_ok: bool,
}

impl RegionAcc {
    fn new(base: u64) -> Self {
        Self {
            base,
            bytes: 0,
            large: false,
            phys_base: None,
            contig: true,
            frames_ok: true,
        }
    }

    fn add(&mut self, mi: &MapInfo) {
        self.bytes += mi.len();
        if mi.len() >= PHYS_LEVEL_LAYOUTS[1].size() {
            self.large = true;
        }
        let implied = mi.paddr().raw().wrapping_sub(mi.vaddr().raw() - self.base);
        match self.phys_base {
            None => self.phys_base = Some(implied),
            Some(phys_base) if phys_base != implied => self.contig = false,
            _ => {}
        }
        if self.contig && self.frames_ok {
            self.frames_ok = get_frame(mi.paddr()).is_some_and(|frame| {
                frame.refcount() == 1 && !frame.is_cow() && !frame.is_wired() && !frame.is_pt()
            });
        }
    }

    fn record(self, out: &mut PromotionCensus) {
        let region = PHYS_LEVEL_LAYOUTS[1].size();
        if self.base == 0 {
            out.region0 += 1;
            out.loose_pages += self.bytes / PageNumber::PAGE_SIZE;
        } else if self.large {
            out.large += 1;
        } else if self.bytes == region {
            let aligned = self
                .phys_base
                .is_some_and(|phys_base| phys_base.is_multiple_of(region as u64));
            if self.contig && aligned {
                if self.frames_ok {
                    out.promotable += 1;
                } else {
                    out.shared += 1;
                }
            } else {
                out.unaligned += 1;
            }
        } else {
            out.partial += 1;
            out.partial_pages += self.bytes / PageNumber::PAGE_SIZE;
            out.loose_pages += self.bytes / PageNumber::PAGE_SIZE;
        }
    }
}
