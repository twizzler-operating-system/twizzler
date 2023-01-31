use crate::arch::{
    address::{PhysAddr, VirtAddr},
    pagetables::EntryFlags,
};

use super::context::MappingPerms;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, PartialOrd, Ord)]
pub enum CacheType {
    #[default]
    WriteBack,
    WriteThrough,
    WriteCombining,
    Uncacheable,
}

pub struct Mapping {
    vaddr_start: VirtAddr,
    paddr_start: PhysAddr,
    length: usize,
    cache_type: CacheType,
    perms: MappingPerms,
}

impl Mapping {
    pub fn new(
        vaddr_start: VirtAddr,
        paddr_start: PhysAddr,
        length: usize,
        perms: MappingPerms,
    ) -> Self {
        Self {
            vaddr_start,
            paddr_start,
            length,
            cache_type: CacheType::default(),
            perms,
        }
    }

    pub fn with_cache_type(mut self, cache_type: CacheType) -> Self {
        self.cache_type = cache_type;
        self
    }

    pub fn vaddr_start(&self) -> VirtAddr {
        self.vaddr_start
    }

    pub fn paddr_start(&self) -> PhysAddr {
        self.paddr_start
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn non_leaf_flags(&self) -> EntryFlags {
        todo!()
    }
}
