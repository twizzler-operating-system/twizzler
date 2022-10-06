use twizzler_abi::device::CacheType;

use crate::memory::{VirtAddr as GenericVirtAddr, PhysAddr as GenericPhysAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArchPhysAddr;

impl ArchPhysAddr {
    pub fn new(_address: u64) -> Self {
        todo!()
    }

    pub fn as_u64(self) -> u64 {
        todo!()
    }

    pub fn align_up<U>(self, _alignment: U) -> Self
    where
        U: Into<u64>
    {
        todo!()
    }

    pub fn align_down<U>(self, _alignment: U) -> Self
    where
        U: Into<u64>
    {
        todo!()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArchVirtAddr;

impl ArchVirtAddr {
    pub fn new(_address: u64) -> Self {
        todo!()
    }

    pub fn as_u64(self) -> u64 {
        todo!()
    }

    pub fn from_ptr<T>(_ptr: *const T) -> Self {
        todo!()
    }

    pub fn as_ptr<T>(self) -> *const T {
        todo!()
    } 

    pub fn as_mut_ptr<T>(self) -> *mut T {
        todo!()
    }

    pub fn align_up<U>(self, _alignment: U) -> Self
    where
        U: Into<u64>
    {
        todo!()
    }

    pub fn align_down<U>(self, _alignment: U) -> Self
    where
        U: Into<u64>
    {
        todo!()
    }
}

use crate::memory::context::MapFlags;
use crate::memory::{MapFailed, MappingInfo};

// start offset into physical memory
const PHYS_MEM_OFFSET: u64 = 0x0;

/* TODO: hide this */
pub fn phys_to_virt(pa: GenericPhysAddr) -> GenericVirtAddr {
    GenericVirtAddr::new(pa.as_u64() + PHYS_MEM_OFFSET)
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Table {
    frame: ArchPhysAddr,
}

impl From<ArchPhysAddr> for Table {
    fn from(frame: ArchPhysAddr) -> Self {
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

    pub fn root(&self) -> ArchPhysAddr {
        self.table_root.frame
    }

    pub fn get_switch_info(&self) -> ArchMemoryContextSwitchInfo {
        todo!()
    }

    pub fn clone_empty_user(&self) -> Self {
        todo!()
    }

    pub fn from_existing_tables(_table_root: GenericPhysAddr) -> Self {
        todo!()
    }

    pub fn current_tables() -> Self {
        todo!()
    }

    pub fn get_map(&self, _va: GenericVirtAddr) -> Option<MappingInfo> {
        todo!()
    }

    #[optimize(speed)]
    pub fn premap(
        &mut self,
        _start: ArchVirtAddr,
        _length: usize,
        _page_size: usize,
        _flags: MapFlags,
    ) -> Result<(), MapFailed> {
        todo!()
    }

    pub fn unmap(&mut self, _start: ArchVirtAddr, _length: usize) {
        /* TODO: Free frames? */
        todo!()
    }

    pub fn map(
        &mut self,
        _start: ArchVirtAddr,
        _phys: ArchPhysAddr,
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
