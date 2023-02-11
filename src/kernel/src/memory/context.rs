use core::sync::atomic::{AtomicUsize, Ordering};

use super::VirtAddr;
use alloc::{collections::BTreeMap, sync::Arc};
use twizzler_abi::object::ObjID;
use twizzler_abi::{device::CacheType, object::Protections};

use crate::{
    arch::memory::{ArchMemoryContext, ArchMemoryContextSwitchInfo},
    idcounter::{Id, IdCounter},
    mutex::{LockGuard, Mutex},
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
pub struct MemoryContextInner {
    pub arch: ArchMemoryContext,
    slots: BTreeMap<usize, MappingRef>,
    thread_count: u64,
}

impl Default for MemoryContextInner {
    fn default() -> Self {
        Self::new()
    }
}

pub struct MemoryContext {
    inner: Mutex<MemoryContextInner>,
    id: Id<'static>,
    switch_cache: ArchMemoryContextSwitchInfo,
    upcall: AtomicUsize,
}

impl Default for MemoryContext {
    fn default() -> Self {
        Self::new()
    }
}

pub type MemoryContextRef = Arc<MemoryContext>;

impl PartialEq for MemoryContext {
    fn eq(&self, other: &Self) -> bool {
        let ida = { self.id.value() };
        let idb = { other.id.value() };
        ida == idb
    }
}

impl Eq for MemoryContext {}

impl PartialOrd for MemoryContext {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        let ida = { self.id.value() };
        let idb = { other.id.value() };
        ida.partial_cmp(&idb)
    }
}

impl Ord for MemoryContext {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let ida = { self.id.value() };
        let idb = { other.id.value() };
        ida.cmp(&idb)
    }
}

bitflags::bitflags! {
    pub struct MappingPerms : u32 {
        const READ = 1;
        const WRITE = 2;
        const EXECUTE = 4;
    }
}

impl From<Protections> for MappingPerms {
    fn from(p: Protections) -> Self {
        let mut s = MappingPerms::empty();
        if p.contains(Protections::READ) {
            s.insert(MappingPerms::READ)
        }
        if p.contains(Protections::WRITE) {
            s.insert(MappingPerms::WRITE)
        }
        if p.contains(Protections::EXEC) {
            s.insert(MappingPerms::EXECUTE)
        }
        s
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
impl MemoryContextInner {
    pub fn new_blank() -> Self {
        Self {
            arch: ArchMemoryContext::new_blank(),
            slots: BTreeMap::new(),
            thread_count: 0,
        }
    }

    pub fn new() -> Self {
        Self {
            // TODO: this is inefficient
            arch: ArchMemoryContext::current_tables().clone_empty_user(),
            slots: BTreeMap::new(),
            thread_count: 0,
        }
    }

    pub fn current() -> Self {
        Self {
            arch: ArchMemoryContext::current_tables(),
            slots: BTreeMap::new(),
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
    pub fn mappings_iter(&self, start: VirtAddr) -> MappingIter {
        MappingIter::new(self, start)
    }

    pub fn lookup_object(&self, addr: VirtAddr) -> Option<MappingRef> {
        self.slots.get(&addr_to_slot(addr)).map(Clone::clone)
    }

    pub fn map_object_page(&mut self, addr: VirtAddr, page: PageRef, perms: MappingPerms) {
        self.arch
            .map(
                addr.align_down(0x1000u64).into(),
                page.physical_address().into(),
                0x1000,
                MapFlags::USER | perms.into(),
                page.cache_type(),
            )
            .unwrap(); //TODO
    }

    pub fn insert_mapping(&mut self, mapping: MappingRef) {
        //TODO: return value
        self.slots.insert(mapping.slot, mapping);
    }

    pub fn clone_region(&mut self, other_ctx: &MemoryContextInner, addr: VirtAddr) {
        for mapping in other_ctx.mappings_iter(addr) {
            //logln!("map {:?}", mapping);
            self.arch
                .map(
                    mapping.addr.into(),
                    mapping.frame.into(),
                    mapping.length,
                    mapping.flags, // | MapFlags::USER, //TODO,
                    CacheType::WriteBack,
                )
                .unwrap();
        }
    }

    pub fn switch(&self) {
        unsafe {
            self.arch.get_switch_info().switch();
        }
    }
}

impl MemoryContext {
    pub fn new_blank() -> Self {
        let inner = Mutex::new(MemoryContextInner::new_blank());
        let switch_cache = { inner.lock().arch.get_switch_info() };
        Self {
            inner,
            switch_cache,
            id: ID_COUNTER.next(),
            upcall: AtomicUsize::new(0),
        }
    }

    pub fn new() -> Self {
        let inner = Mutex::new(MemoryContextInner::new());
        let switch_cache = { inner.lock().arch.get_switch_info() };
        Self {
            inner,
            switch_cache,
            id: ID_COUNTER.next(),
            upcall: AtomicUsize::new(0),
        }
    }

    pub fn current() -> Self {
        let inner = Mutex::new(MemoryContextInner::current());
        let switch_cache = { inner.lock().arch.get_switch_info() };
        Self {
            inner,
            switch_cache,
            id: ID_COUNTER.next(),
            upcall: AtomicUsize::new(0),
        }
    }

    pub fn switch(&self) {
        unsafe {
            self.switch_cache.switch();
        }
    }

    pub fn inner(&self) -> LockGuard<'_, MemoryContextInner> {
        self.inner.lock()
    }

    pub fn set_upcall_address(&self, target: usize) {
        self.upcall.store(target, Ordering::SeqCst);
    }

    pub fn get_upcall_address(&self) -> Option<usize> {
        match self.upcall.load(Ordering::SeqCst) {
            0 => None,
            n => Some(n),
        }
    }
}

use crate::syscall::object::ObjectHandle;
impl ObjectHandle for MemoryContextRef {
    fn create_with_handle(_obj: ObjectRef) -> Self {
        Arc::new(MemoryContext::new())
    }
}

/// A trait that defines the operations expected by higher-level object management routines. An architecture-dependent
/// type can be created that implements Context, which can then be used by the rest of the kernel to manage objects in a
/// context (e.g. an address space).
trait Context {
    /// The type that is expected for upcall information (e.g. an entry address).
    type UpcallInfo;

    /// Set the context's upcall information.
    fn set_upcall(&self, target: Self::UpcallInfo);
    /// Retrieve the context's upcall information.
    fn get_upcall(&self) -> Option<Self::UpcallInfo>;
    /// Switch to this context.
    fn switch_to(&self);
    /// Insert a range of an object into the context. The implementation may choose to use start and len as hints, but
    /// should keep in mind that calls to `insert_object` may be generated by faults, and so should strive to resolve
    /// the fault by correctly mapping the object as requested.
    fn insert_object(
        &self,
        obj: ObjectRef,
        start: usize,
        len: usize,
        perms: MappingPerms,
        cache: CacheType,
    );
    /// Remove an object's mapping from the context.
    fn remove_object(&self, obj: ObjID, start: usize, len: usize);
    /// Write protect a region of the object's mapping. For correctness, the implementation must ensure that the region
    /// is, indeed, write protected. If this means protecting the entire object, so be it.
    fn write_protect(&self, obj: ObjID, start: usize, len: usize);
}
