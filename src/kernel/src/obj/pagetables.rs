use alloc::vec::{self, Vec};

use itertools::Itertools;
use twizzler_abi::device::CacheType;
use twizzler_rt_abi::error::TwzError;

use crate::{
    arch::{PhysAddr, VirtAddr, memory::pagetables::ArchTlbMgr},
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, get_frame, min_level_for_len},
        pagetables::{
            Consistency, ContiguousProvider, DeferredUnmappingOps, MapInfo, MapReader, Mapper,
            MappingCursor, MappingSettings, Table,
        },
        tracker::{FrameAllocFlags, alloc_frame, free_frame},
    },
    obj::{Object, ObjectRef, PageNumber},
};

const MAX_INVL_TARGETS: usize = 8;
const MAX_INVLS: usize = 4;

pub struct ObjectPageTable {
    mapper: Mapper,
    invls: heapless::Vec<(PhysAddr, heapless::Vec<MappingCursor, MAX_INVLS>), MAX_INVL_TARGETS>,
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
        let _ = self.mapper.unmap(cursor, &mut consist);
        self.run_consistency(consist).run_all();
        let root_frame = get_frame(self.mapper.root_address()).expect("root frame should exist");
        root_frame.set_pt(false);
        root_frame.dec_refcount();
        free_frame(root_frame);
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
        mapper.set_start_level(Table::top_level() - 2);
        Self {
            mapper,
            invls: heapless::Vec::new(),
        }
    }

    pub fn map_count(&self) -> usize {
        let root_addr = self.mapper.root_address();
        let frame = get_frame(root_addr).expect("root frame should exist");
        frame.refcount() as usize
    }

    pub fn add_invalidate(&mut self, target: PhysAddr, cursor: MappingCursor) {
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

    pub fn remove_invalidate(&mut self, target: PhysAddr, cursor: MappingCursor) {
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
        PHYS_LEVEL_LAYOUTS[self.mapper.start_level() + 1].size()
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
                    min_level_for_len(len).unwrap_or(self.mapper.start_level() + 1),
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
        let tlb = if !consist.tlb().is_full() {
            if consist.tlb().has_pending() {
                let mut final_tlb: Option<ArchTlbMgr> = None;
                'out: for (target, maps) in self.invls.iter() {
                    if maps.is_empty() {
                        continue;
                    }

                    consist.tlb_mut().set_target(*target);

                    for map in maps.iter() {
                        // For each map, copy, offset, and merge.
                        let tlb = consist.tlb().apply_offset_from_map(map);

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
        } else {
            Some(ArchTlbMgr::new_full_global())
        };
        consist.tlb_mut().reset();
        tlb
    }

    pub fn map_page(&mut self, offset: u64, page: FrameRef) -> Result<(), TwzError> {
        let mut consist = Consistency::new_object_tables();
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), page.size());
        let mut phys = ContiguousProvider::new(
            page.start_address(),
            page.size(),
            MappingSettings::default_user(),
        );
        let r = self.mapper.map(cursor, &mut phys, &mut consist);
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
        let cursor = MappingCursor::new(
            VirtAddr::new(0).unwrap(),
            Table::level_to_page_size(self.mapper.start_level() + 1),
        );
        let reader = self.mapper.readmap(cursor).coalesce();
        reader.fold(0, |acc, mi| {
            if mi.is_empty() {
                acc
            } else {
                acc + mi.len() / PageNumber::PAGE_SIZE
            }
        })
    }

    pub fn get_dirty_and_reset(&mut self) -> Result<Vec<(PageNumber, PhysAddr, usize)>, TwzError> {
        let cursor = MappingCursor::new(
            VirtAddr::new(0).unwrap(),
            Table::level_to_page_size(self.mapper.start_level() + 1),
        );

        fn add_to_list(dirty_list: &mut Vec<(PageNumber, PhysAddr, usize)>, mi: &MapInfo) {
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

            if let Some(pos) = dirty_list.iter().position(|item| can_append(mi, item)) {
                dirty_list[pos].2 += mi.len() / PageNumber::PAGE_SIZE;
            } else {
                dirty_list.push((
                    PageNumber::from_address(mi.vaddr()),
                    mi.paddr(),
                    mi.len() / PageNumber::PAGE_SIZE,
                ));
            }
        }

        let mut consist = Consistency::new_object_tables();
        let mut dirty_list = vec::Vec::new();
        let r = self.mapper.with_dirty_bits(
            cursor,
            |mi| {
                add_to_list(&mut dirty_list, &mi);
                true
            },
            &mut consist,
        );

        self.run_consistency(consist).run_all();

        r?;
        Ok(dirty_list)
    }

    pub fn maybe_cow_at(&mut self, offset: u64, mark_dirty: bool) -> Result<bool, TwzError> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());

        let mut consist = Consistency::new_object_tables();
        let did_cow = self.mapper.cow_at(cursor, &mut consist, mark_dirty);

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
        let page_aligned_offset = offset & !(PHYS_LEVEL_LAYOUTS[0].size() as u64 - 1);
        let mut map_info = reader.next();
        if map_info
            .as_ref()
            .is_some_and(|mi| mi.vaddr().raw() != page_aligned_offset)
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
        let page_aligned_offset = offset & !(PHYS_LEVEL_LAYOUTS[0].size() as u64 - 1);
        reader
            .next()
            .filter(|x| x.vaddr().raw() == page_aligned_offset)
    }

    pub fn split_to_level(&mut self, offset: u64, level: usize) -> Result<(), TwzError> {
        let mut consist = Consistency::new_object_tables();
        let r = self
            .mapper
            .split_to_level(VirtAddr::new(offset).unwrap(), level, &mut consist);
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
        self.mapper
            .setup_cow_range(&mut dest.mapper, src_cursor, dst_cursor, &mut consist)?;
        self.run_consistency2(consist, dest).run_all();
        Ok(())
    }

    pub fn setup_zero_range(&mut self, offset: u64, len: usize) -> Result<(), TwzError> {
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), len);
        let mut consist = Consistency::new_object_tables();
        let ops = self.mapper.setup_zero_range(cursor, &mut consist);
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
        let mut phys =
            ContiguousProvider::new(start, len, MappingSettings::default_user().with_cache(ct));
        let mut consist = Consistency::new_object_tables();
        let r = pt.mapper.map(cursor, &mut phys, &mut consist);
        pt.run_consistency(consist).run_all();
        r
    }

    pub fn add_frame(&self, pn: PageNumber, frame: FrameRef) {
        let mut pt = self.lock_page_tables();
        pt.map_page(pn.as_byte_offset() as u64, frame).unwrap();
    }

    pub fn cow_clone_page_tables(self: &ObjectRef) -> Result<ObjectPageTable, TwzError> {
        let mut new_pt = ObjectPageTable::new();
        let old_pt = self.lock_page_tables();
        let level = old_pt.mapper.start_level();
        assert_eq!(level, new_pt.mapper.start_level());
        let cursor = MappingCursor::new(
            VirtAddr::new(0).unwrap(),
            Table::level_to_page_size(level + 1),
        );
        let mut consist = Consistency::new_object_tables();
        let mut old_pt = self.ensure_in_core(
            old_pt,
            PageNumber::from(0),
            cursor.remaining() / PageNumber::PAGE_SIZE,
            &mut false,
            &mut false,
        )?;
        let r = old_pt
            .mapper
            .setup_cow_range(&mut new_pt.mapper, cursor, cursor, &mut consist);
        old_pt.run_consistency(consist).run_all();
        r.map(|_| new_pt)
    }
}
