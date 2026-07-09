use alloc::vec::{self, Vec};

use bitset_core::BitSet;
use itertools::Itertools;
use twizzler_abi::device::CacheType;
use twizzler_rt_abi::error::{ObjectError, ResourceError, TwzError};

use crate::{
    arch::{PhysAddr, VirtAddr, memory::pagetables::ArchTlbMgr},
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, get_frame, min_level_for_len},
        pagetables::{
            Consistency, ContiguousProvider, MapInfo, MapReader, Mapper, MappingCursor,
            MappingSettings, Table,
        },
        tracker::{FrameAllocFlags, alloc_frame},
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

impl ObjectPageTable {
    pub fn new() -> Self {
        let frame = alloc_frame(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
        );
        frame.set_pt(true);
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
        PHYS_LEVEL_LAYOUTS[self.mapper.start_level()].size()
    }

    pub fn invalidate(&mut self, offset: u64, len: usize) {
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

    pub fn map_page(&mut self, offset: u64, page: FrameRef) -> bool {
        let consist = Consistency::new_full_global();
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), page.size());
        let mut phys = ContiguousProvider::new(
            page.start_address(),
            page.size(),
            MappingSettings::default_user(),
        );
        if let Err(e) = self.mapper.map(cursor, &mut phys, consist) {
            e.run_all();
            return false;
        }
        self.invalidate_page(PageNumber::from_offset(offset as usize));
        // TODO: mark dirty

        true
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
            Table::level_to_page_size(self.mapper.start_level()),
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

    pub fn get_dirty_and_reset(&mut self) -> Vec<(PageNumber, PhysAddr, usize)> {
        let cursor = MappingCursor::new(
            VirtAddr::new(0).unwrap(),
            Table::level_to_page_size(self.mapper.start_level()),
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

        let mut dirty_list = vec::Vec::new();
        let any = self.mapper.with_dirty_bits(cursor, |mi| {
            add_to_list(&mut dirty_list, &mi);
            true
        });
        if any {
            if dirty_list.len() == 1 && dirty_list[0].2 == 1 {
                self.invalidate_page(dirty_list[0].0);
            } else {
                self.invalidate_full();
            }
        }
        dirty_list
    }

    pub fn maybe_cow_at(&mut self, offset: u64) -> Result<bool, TwzError> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        // TODO: handle invalidations?
        let did_cow = self
            .mapper
            .cow_at(cursor)
            .ok_or(ResourceError::OutOfMemory.into());

        if did_cow.is_ok_and(|dc| dc) {
            self.invalidate_page(PageNumber::from_offset(offset as usize));
        }

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
            *did_cow = self.maybe_cow_at(offset)?;
            // TODO: mark dirty
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
        self.mapper
            .split_to_level(VirtAddr::new(offset).unwrap(), level)
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
        self.mapper
            .setup_cow_range(&mut dest.mapper, src_cursor, dst_cursor)
    }

    pub fn setup_zero_range(&mut self, offset: u64, len: usize) -> Result<(), TwzError> {
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), len);
        let ops = self.mapper.unmap(cursor);
        ops.run_all();
        Ok(())
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
        let consist = Consistency::new_full_global();
        if let Err(e) = pt.mapper.map(cursor, &mut phys, consist) {
            e.run_all();
            return Err(ObjectError::MapFailed.into());
        }
        pt.invalidate(offset as u64, end - start);
        Ok(())
    }

    pub fn add_frame(&self, pn: PageNumber, frame: FrameRef) {
        let mut pt = self.lock_page_tables();
        pt.map_page(pn.as_byte_offset() as u64, frame);
    }

    pub fn cow_clone_page_tables(self: &ObjectRef) -> Result<ObjectPageTable, TwzError> {
        let mut new_pt = ObjectPageTable::new();
        let old_pt = self.lock_page_tables();
        let level = old_pt.mapper.start_level();
        assert_eq!(level, new_pt.mapper.start_level());
        let cursor =
            MappingCursor::new(VirtAddr::new(0).unwrap(), Table::level_to_page_size(level));
        let mut old_pt = self.ensure_in_core(
            old_pt,
            PageNumber::from(0),
            cursor.remaining() / PageNumber::PAGE_SIZE,
            &mut false,
            &mut false,
        )?;
        old_pt
            .mapper
            .setup_cow_range(&mut new_pt.mapper, cursor, cursor)?;
        Ok(new_pt)
    }
}

#[derive(Clone)]
pub struct DirtySet {
    set: Vec<u8>,
}

impl DirtySet {
    pub fn new() -> Self {
        Self { set: Vec::new() }
    }

    pub fn drain_all(&mut self) -> Vec<(PageNumber, usize)> {
        let mut pages: Vec<(PageNumber, usize)> = Vec::new();
        for b in 0..self.set.bit_len() {
            if self.set.bit_test(b) {
                if b > 0 && self.set.bit_test(b - 1) {
                    pages.last_mut().unwrap().1 += 1;
                } else {
                    pages.push((PageNumber::from(b), 1));
                }
            }
        }
        self.set.fill(0);
        pages
    }

    fn is_dirty(&self, pn: PageNumber) -> bool {
        if pn.0 < self.set.bit_len() {
            self.set.bit_test(pn.0)
        } else {
            false
        }
    }

    pub fn add_dirty(&mut self, pn: PageNumber, num: usize) {
        if pn.0 + num > self.set.bit_len() {
            let add = ((pn.0 + num) - self.set.bit_len()) / 8 + 1;
            self.set.extend((0..add).into_iter().map(|_| 0));
        }
        for i in 0..num {
            self.set.bit_set(pn.0 + i);
        }
    }

    pub fn add_dirty_cursor(&mut self, cursor: MappingCursor) {
        let start = PageNumber::from_address(cursor.start());
        let len = cursor.remaining() / PageNumber::PAGE_SIZE;
        self.add_dirty(start, len);
    }

    fn reset_dirty(&mut self, pn: PageNumber) {
        if pn.0 < self.set.bit_len() {
            self.set.bit_reset(pn.0);
        }
    }
}
