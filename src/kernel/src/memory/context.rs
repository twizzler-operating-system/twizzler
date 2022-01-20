use alloc::{collections::BTreeMap, sync::Arc};
use x86_64::VirtAddr;

use crate::{
    arch::memory::ArchMemoryContext,
    idcounter::{Id, IdCounter},
    mutex::Mutex,
    obj::{pages::PageRef, ObjectRef},
};

#[derive(Ord, PartialOrd, PartialEq, Eq)]
pub struct Mapping {
    pub obj: ObjectRef,
    pub perms: MappingPerms,
    pub vmc: MemoryContextRef,
    pub slot: usize,
}

pub type MappingRef = Arc<Mapping>;

impl Mapping {
    pub fn new(obj: ObjectRef, vmc: MemoryContextRef, slot: usize, perms: MappingPerms) -> Self {
        Self {
            obj,
            vmc,
            slot,
            perms,
        }
    }
}

use super::MappingIter;
pub struct MemoryContext {
    pub arch: ArchMemoryContext,
    slots: BTreeMap<usize, MappingRef>,
    id: Id<'static>,
    thread_count: u64,
}

pub type MemoryContextRef = Arc<Mutex<MemoryContext>>;

impl PartialEq for MemoryContext {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for MemoryContext {}

impl PartialOrd for MemoryContext {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for MemoryContext {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

impl crate::idcounter::StableId for MemoryContext {
    fn id(&self) -> &crate::idcounter::Id<'_> {
        &self.id
    }
}

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

static ID_COUNTER: IdCounter = IdCounter::new();
impl MemoryContext {
    pub fn new_blank() -> Self {
        Self {
            arch: ArchMemoryContext::new_blank(),
            slots: BTreeMap::new(),
            id: ID_COUNTER.next(),
            thread_count: 0,
        }
    }

    pub fn new() -> Self {
        Self {
            // TODO: this is inefficient
            arch: ArchMemoryContext::current_tables().clone_empty_user(),
            slots: BTreeMap::new(),
            id: ID_COUNTER.next(),
            thread_count: 0,
        }
    }

    pub fn current() -> Self {
        Self {
            arch: ArchMemoryContext::current_tables(),
            slots: BTreeMap::new(),
            id: ID_COUNTER.next(),
            thread_count: 0,
        }
    }

    fn clear_mappings(&mut self) {
        self.slots.clear();
    }

    pub fn add_thread(&mut self) {
        self.thread_count += 1;
    }

    pub fn remove_thread(&mut self) {
        self.thread_count -= 1;
        if self.thread_count == 0 {
            self.clear_mappings();
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

    pub fn lookup_object(&self, addr: VirtAddr) -> Option<MappingRef> {
        self.slots.get(&addr_to_slot(addr)).map(Clone::clone)
    }

    pub fn map_object_page(&mut self, addr: VirtAddr, page: PageRef, perms: MappingPerms) {
        self.arch
            .map(
                addr.align_down(0x1000u64),
                page.physical_address(),
                0x1000,
                MapFlags::USER | perms.into(),
            )
            .unwrap(); //TODO
    }

    pub fn insert_mapping(&mut self, mapping: MappingRef) {
        //TODO: return value
        self.slots.insert(mapping.slot, mapping);
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
