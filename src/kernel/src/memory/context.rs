use alloc::{collections::BTreeMap, sync::Arc};
use x86_64::{
    structures::paging::{FrameAllocator, Size4KiB},
    VirtAddr,
};

use crate::{
    arch::memory::{ArchMemoryContext, MapFlags},
    mutex::Mutex,
    obj::ObjectRef,
};

use super::MappingIter;
pub struct MemoryContext {
    pub arch: ArchMemoryContext,
    slots: BTreeMap<usize, ObjectRef>,
}

pub type MemoryContextRef = Arc<Mutex<MemoryContext>>;

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
        logln!("switching contexts");
        unsafe {
            self.arch.switch();
        }
    }

    pub fn mappings_iter(&self, start: VirtAddr) -> MappingIter {
        MappingIter::new(self, start)
    }

    pub fn lookup_object(&self, addr: VirtAddr) -> Option<ObjectRef> {
        self.slots.get(&addr_to_slot(addr)).map(Clone::clone)
    }

    pub fn clone_region(&mut self, other_ctx: &MemoryContext, addr: VirtAddr) {
        for mapping in other_ctx.mappings_iter(addr) {
            self.arch
                .map(
                    mapping.addr,
                    mapping.frame,
                    mapping.length,
                    mapping.flags | MapFlags::USER,
                )
                .unwrap();
        }
    }
}
