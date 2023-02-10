use twizzler_abi::device::CacheType;

use crate::memory::context::MapFlags;
use crate::memory::{VirtAddr, PhysAddr, MapFailed, MappingInfo};

// start offset into physical memory
const PHYS_MEM_OFFSET: u64 = 0x0;

/* TODO: hide this */
pub fn phys_to_virt(_pa: PhysAddr) -> VirtAddr {
    todo!()
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Table {
    frame: PhysAddr,
}

impl From<PhysAddr> for Table {
    fn from(frame: PhysAddr) -> Self {
        Self { frame }
    }
}
pub struct ArchMemoryContext {
    table_root: Table,
}

pub struct ArchMemoryContextSwitchInfo {
    target: u64,
}

// arch specific page sizes supported
const PAGE_SIZE_HUGE: usize = 1024 * 1024 * 1024;
const PAGE_SIZE_LARGE: usize = 2 * 1024 * 1024;
const PAGE_SIZE: usize = 0x1000;

impl ArchMemoryContext {
    pub fn new_blank() -> Self {
        todo!()
    }

    pub fn root(&self) -> PhysAddr {
        self.table_root.frame
    }

    pub fn get_switch_info(&self) -> ArchMemoryContextSwitchInfo {
        todo!()
    }

    pub fn clone_empty_user(&self) -> Self {
        todo!()
    }

    pub fn from_existing_tables(_table_root: PhysAddr) -> Self {
        todo!()
    }

    pub fn current_tables() -> Self {
        todo!()
    }

    pub fn get_map(&self, _va: VirtAddr) -> Option<MappingInfo> {
        todo!()
    }

    #[optimize(speed)]
    pub fn premap(
        &mut self,
        _start: VirtAddr,
        _length: usize,
        _page_size: usize,
        _flags: MapFlags,
    ) -> Result<(), MapFailed> {
        todo!()
    }

    pub fn unmap(&mut self, _start: VirtAddr, _length: usize) {
        /* TODO: Free frames? */
        todo!()
    }

    pub fn map(
        &mut self,
        _start: VirtAddr,
        _phys: PhysAddr,
        mut _length: usize,
        _flags: MapFlags,
        _cache_type: CacheType,
    ) -> Result<(), MapFailed> {
        todo!()
    }
}

impl ArchMemoryContextSwitchInfo {
    /// Switch context.
    /// # Safety
    /// The context must be valid.
    pub unsafe fn switch(&self) {
        todo!()
    }
}

pub unsafe fn flush_tlb() {
    todo!()
}
