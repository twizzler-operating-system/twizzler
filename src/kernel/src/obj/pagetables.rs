use alloc::vec::Vec;

use itertools::Itertools;
use twizzler_abi::device::CacheType;
use twizzler_rt_abi::error::TwzError;

use crate::{
    arch::{PhysAddr, VirtAddr, context::ArchContextTarget, memory::pagetables::ArchTlbMgr},
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, get_frame, min_level_for_len},
        pagetables::{
            Consistency, ContiguousProvider, DeferredUnmappingOps, MapInfo, MapReader, Mapper,
            MappingCursor, MappingSettings, Table,
        },
        tracker::{
            FrameAllocFlags, FrameAllocator, alloc_frame, free_frame, take_or_new_frame_allocator,
        },
    },
    obj::{Object, ObjectRef, PageNumber},
};

const MAX_INVL_TARGETS: usize = 8;
const MAX_INVLS: usize = 4;

/// What became of a page the pager delivered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageInstall {
    Installed,
    /// A 4 KiB page was already there.
    PresentSmall,
    /// A large page already covers the offset. Worth counting apart from the small case: one
    /// overlapping request that loses the race to a merge has every one of its pages in that
    /// 2 MiB region rejected, so a handful of merges can account for a great many duplicates.
    PresentLarge,
}

pub struct ObjectPageTable {
    mapper: Mapper,
    invls: heapless::Vec<
        (ArchContextTarget, heapless::Vec<MappingCursor, MAX_INVLS>),
        MAX_INVL_TARGETS,
    >,
    map_count: usize,
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
        self.run_consistency(consist).run_all();
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
        }
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
            tlb.finish();
            return;
        }
        for (target, maps) in self.invls.iter() {
            if maps.is_empty() {
                continue;
            }

            let mut tlb = ArchTlbMgr::new(*target);
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
            tlb.finish();
        }
    }

    pub fn invalidate_page(&mut self, pn: PageNumber) {
        let offset = pn.as_byte_offset() as u64;
        let len = PageNumber::PAGE_SIZE;
        self.invalidate(offset, len);
    }

    pub fn invalidate_full(&mut self) {
        self.invalidate(0, self.max_len());
    }

    pub fn run_consistency2(&self, mut consist: Consistency, other: &Self) -> DeferredUnmappingOps {
        let tlb = self.do_run_consistency(&mut consist);
        let other_tlb = other.do_run_consistency(&mut consist);
        match (tlb, other_tlb) {
            (Some(mut tlb), Some(other_tlb)) => {
                tlb.merge(other_tlb);
                tlb.finish();
            }
            (Some(mut tlb), None) => {
                tlb.finish();
            }
            (None, Some(mut other_tlb)) => {
                other_tlb.finish();
            }
            (None, None) => {}
        }
        consist.into_deferred()
    }

    pub fn run_consistency(&self, mut consist: Consistency) -> DeferredUnmappingOps {
        let tlb = self.do_run_consistency(&mut consist);
        if let Some(mut tlb) = tlb {
            tlb.finish();
        }
        consist.into_deferred()
    }

    pub fn do_run_consistency(&self, consist: &mut Consistency) -> Option<ArchTlbMgr> {
        // `add_invalidate` drops silently once its bounded lists fill, so past MAX_INVL_TARGETS
        // contexts (or MAX_INVLS cursors within one) this object no longer knows where all of its
        // mappings live. Retargeting precisely would then reach only the contexts that happened to
        // fit and skip the rest entirely -- the same reason `invalidate` gives up and goes global.
        let overflowed = self.invls.is_full() || self.invls.iter().any(|(_, maps)| maps.is_full());
        let tlb = if !consist.tlb().is_full() && !overflowed {
            if consist.tlb().has_pending() {
                let mut final_tlb: Option<ArchTlbMgr> = None;
                'out: for (target, maps) in self.invls.iter() {
                    if maps.is_empty() {
                        continue;
                    }

                    consist.tlb_mut().set_target(*target);

                    for map in maps.iter() {
                        // For each map, copy, offset, and merge.
                        let mut tlb = consist.tlb().apply_offset_from_map(map);
                        // See Self::invalidate: a kernel-range mapping is visible under every PCID,
                        // so precise invalidation cannot reach all of its copies.
                        if map.start().is_kernel() {
                            tlb.set_full_global();
                        }

                        if let Some(ref mut final_tlb) = final_tlb {
                            final_tlb.merge(tlb);
                        } else {
                            final_tlb = Some(tlb);
                        }

                        if final_tlb.as_ref().unwrap().is_full() {
                            break 'out;
                        }
                    }
                }
                final_tlb
            } else {
                None
            }
        } else if consist.tlb().has_pending() {
            Some(ArchTlbMgr::new_full_global())
        } else {
            None
        };
        consist.tlb_mut().reset();
        tlb
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
        self.run_consistency(consist).run_all();
        r
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

        self.run_consistency(consist).run_all();

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

        self.run_consistency(consist).run_all();

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
        self.run_consistency(consist).run_all();
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
        self.run_consistency2(consist, dest).run_all();
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
        self.run_consistency(consist).run_all();
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
        pt.run_consistency(consist).run_all();
        r
    }

    pub fn add_frame(&self, pn: PageNumber, frame: FrameRef) {
        let mut pt = self.lock_page_tables();
        pt.map_page(pn.as_byte_offset() as u64, frame).unwrap();
    }

    /// Install `frame` at `pn`, unless the object already has a page there. Reports which of the
    /// two happened, and if the page was lost, to what.
    ///
    /// The pager can deliver a page the object acquired between the request being issued and the
    /// completion landing; two overlapping in-flight requests produce those by construction, since
    /// `add_request` coalesces only on an exact range. `Table::map` already declines to overwrite a
    /// present entry, so the frame was dropped either way -- what this skips is doing the whole
    /// mapping walk to find that out: a frame allocator, a precharge, a consistency pass and its
    /// invalidation, per duplicate page.
    pub fn add_frame_if_absent(&self, pn: PageNumber, frame: FrameRef) -> PageInstall {
        let mut pt = self.lock_page_tables();
        let offset = pn.as_byte_offset() as u64;
        if !pt.is_empty_at_level(offset, 0) {
            // Only walked for a page that is already lost, so this second walk costs nothing that
            // was going to be useful, and it is the only way to tell the two apart:
            // `is_empty_at_level` reports a large-page leaf and a present 4 KiB entry alike.
            let large = pt
                .readmap(offset, PageNumber::PAGE_SIZE)
                .next()
                .is_some_and(|info| info.len() > PageNumber::PAGE_SIZE);
            return if large {
                PageInstall::PresentLarge
            } else {
                PageInstall::PresentSmall
            };
        }
        pt.map_page(offset, frame).unwrap();
        PageInstall::Installed
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
        old_pt.run_consistency(consist).run_all();
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
