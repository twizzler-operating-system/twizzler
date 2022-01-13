use alloc::{collections::BTreeMap, sync::Arc};
use x86_64::{
    structures::paging::{FrameAllocator, Size4KiB},
    VirtAddr,
};

use crate::{
    arch::memory::ArchMemoryContext,
    mutex::Mutex,
    obj::{pages::PageRef, ObjectRef},
};

use super::MappingIter;
pub struct MemoryContext {
    pub arch: ArchMemoryContext,
    slots: BTreeMap<usize, (ObjectRef, MappingPerms)>,
}

pub type MemoryContextRef = Arc<Mutex<MemoryContext>>;

bitflags::bitflags! {
    pub struct MappingPerms : u32 {
        const READ = 1;
        const WRITE = 2;
        const EXECUTE = 4;
    }
}

bitflags::bitflags! {
    pub struct MapFlags: u64 {
        const READ= 0x1;
        const WRITE= 0x2;
        const EXECUTE= 0x4;
        const USER= 0x8;
        const GLOBAL= 0x10;
        const WIRED = 0x20;
    }
}

impl From<MappingPerms> for MapFlags {
    fn from(mp: MappingPerms) -> Self {
        let mut flags = MapFlags::empty();
        if mp.contains(MappingPerms::READ) {
            flags.insert(MapFlags::READ);
        }
        if mp.contains(MappingPerms::WRITE) {
            flags.insert(MapFlags::WRITE);
        }
        if mp.contains(MappingPerms::EXECUTE) {
            flags.insert(MapFlags::EXECUTE);
        }
        flags
    }
}

pub fn addr_to_slot(addr: VirtAddr) -> usize {
    (addr.as_u64() / (1 << 30)) as usize //TODO: arch-dep
}

impl MemoryContext {
    pub fn new_blank() -> Self {
        Self {
            arch: ArchMemoryContext::new_blank(),
            slots: BTreeMap::new(),
        }
    }

    pub fn new() -> Self {
        Self {
            // TODO: this is inefficient
            arch: ArchMemoryContext::current_tables().clone_empty_user(),
            slots: BTreeMap::new(),
        }
    }

    pub fn current() -> Self {
        Self {
            arch: ArchMemoryContext::current_tables(),
            slots: BTreeMap::new(),
        }
    }

    pub fn switch(&self) {
        unsafe {
            self.arch.switch();
        }
    }

    pub fn mappings_iter(&self, start: VirtAddr) -> MappingIter {
        MappingIter::new(self, start)
    }

    pub fn lookup_object(&self, addr: VirtAddr) -> Option<(ObjectRef, MappingPerms)> {
        self.slots.get(&addr_to_slot(addr)).map(Clone::clone)
    }

    pub fn map_object_page(&mut self, addr: VirtAddr, page: PageRef, perms: MappingPerms) {
        self.arch.map(
            addr.align_down(0x1000u64),
            page.physical_address(),
            0x1000,
            MapFlags::USER | perms.into(),
        );
    }

    pub fn map_object(&mut self, slot: usize, obj: ObjectRef, perms: MappingPerms) {
        //TODO: return value
        self.slots.insert(slot, (obj, perms));
    }

    pub fn clone_region(&mut self, other_ctx: &MemoryContext, addr: VirtAddr) {
        for mapping in other_ctx.mappings_iter(addr) {
            self.arch
                .map(
                    mapping.addr,
                    mapping.frame,
                    mapping.length,
                    mapping.flags | MapFlags::USER, //TODO,
                )
                .unwrap();
        }
    }
}
