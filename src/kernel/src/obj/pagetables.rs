use alloc::sync::Arc;

use twizzler_abi::device::CacheType;
use twizzler_rt_abi::error::{ObjectError, ResourceError, TwzError};

use crate::{
    arch::{PhysAddr, VirtAddr},
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, get_frame},
        pagetables::{
            Consistency, ContiguousProvider, MapReader, Mapper, MappingCursor, MappingSettings,
            Table,
        },
        tracker::{FrameAllocFlags, alloc_frame},
    },
    mutex::Mutex,
    obj::{Object, PageNumber},
};

pub struct ObjectPageTable {
    mapper: Mapper,
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
        Self { mapper }
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
        todo!()
    }

    pub fn maybe_cow_at(&mut self, offset: u64) -> Result<bool, TwzError> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        // TODO: handle invalidations?
        self.mapper
            .cow_at(cursor)
            .ok_or(ResourceError::OutOfMemory.into())
    }

    pub fn with_frame<R>(
        &mut self,
        offset: u64,
        flags: FindFrameFlags,
        f: impl FnOnce(usize, Option<FrameRef>) -> R,
    ) -> Result<R, TwzError> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        if flags.contains(FindFrameFlags::WRITE) {
            self.maybe_cow_at(offset)?;
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

    pub fn get_frame(&mut self, offset: u64) -> Option<FrameRef> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        let mut reader = self.mapper.readmap(cursor);
        let page_aligned_offset = offset & !(PHYS_LEVEL_LAYOUTS[0].size() as u64 - 1);
        reader.next().and_then(|x| {
            if x.vaddr().raw() == page_aligned_offset {
                get_frame(x.paddr())
            } else {
                None
            }
        })
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
        Ok(())
    }

    pub fn add_frame(&self, pn: PageNumber, frame: FrameRef) {
        let mut pt = self.lock_page_tables();
        pt.map_page(pn.as_byte_offset() as u64, frame);
    }
}
