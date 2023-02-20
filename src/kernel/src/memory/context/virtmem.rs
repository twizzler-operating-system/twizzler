use core::ptr::NonNull;

use alloc::collections::BTreeMap;
use twizzler_abi::object::{ObjID, MAX_SIZE};

use super::{InsertError, KernelMemoryContext, MappingPerms, ObjectContextInfo, UserContext};
use crate::{
    arch::{address::VirtAddr, context::ArchContext},
    memory::{
        map::CacheType,
        pagetables::{
            ContiguousProvider, Mapper, MappingCursor, MappingFlags, MappingSettings,
            PhysAddrProvider, ZeroPageProvider,
        },
    },
    mutex::Mutex,
    obj::ObjectRef,
    spinlock::Spinlock,
};

use crate::{
    obj::{pages::Page, PageNumber},
    thread::{current_memory_context, current_thread_ref},
};

/// A type that implements [Context] for virtual memory systems.
pub struct VirtContext {
    arch: ArchContext,
    upcall: Spinlock<Option<VirtAddr>>,
    slots: Mutex<SlotMgr>,
}

#[derive(Default)]
struct SlotMgr {
    slots: BTreeMap<Slot, VirtContextSlot>,
    objs: BTreeMap<ObjID, Slot>,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct Slot(usize);

impl Slot {
    fn start_vaddr(&self) -> VirtAddr {
        // TODO
        VirtAddr::new((self.0 * MAX_SIZE) as u64).unwrap()
    }

    fn raw(&self) -> usize {
        self.0
    }
}

impl TryFrom<VirtAddr> for Slot {
    type Error = ();

    fn try_from(value: VirtAddr) -> Result<Self, Self::Error> {
        if value.is_kernel() {
            Err(())
        } else {
            // TODO
            Ok(Self((value.raw() / MAX_SIZE as u64) as usize))
        }
    }
}

impl SlotMgr {
    fn get(&self, slot: Slot) -> Option<&VirtContextSlot> {
        self.slots.get(&slot)
    }

    fn insert(&mut self, slot: Slot, id: ObjID, info: VirtContextSlot) {
        self.slots.insert(slot, info);
        self.objs.insert(id, slot);
    }

    fn remove(&mut self, slot: Slot) {
        if let Some(info) = self.slots.remove(&slot) {
            self.objs.remove(&info.obj.id());
        }
    }

    fn obj_to_slot(&self, id: ObjID) -> Option<Slot> {
        self.objs.get(&id).cloned()
    }
}

struct ObjectPageProvider {
    obj: ObjectRef,
}

impl PhysAddrProvider for ObjectPageProvider {
    fn peek(&mut self) -> (crate::arch::address::PhysAddr, usize) {
        todo!()
    }

    fn consume(&mut self, _len: usize) {
        todo!()
    }
}

impl VirtContext {
    fn map_slot(&self, slot: Slot, start: usize, len: usize) {
        let slots = self.slots.lock();
        if let Some(info) = slots.get(slot) {
            let mut phys = info.phys_provider();
            self.arch.map(
                info.mapping_cursor(start, len),
                &mut phys,
                &info.mapping_settings(false),
            );
        }
    }

    fn wp_slot(&self, slot: Slot, start: usize, len: usize) {
        let slots = self.slots.lock();
        if let Some(info) = slots.get(slot) {
            self.arch.change(
                info.mapping_cursor(start, len),
                &info.mapping_settings(true),
            );
        }
    }

    fn __new(arch: ArchContext) -> Self {
        Self {
            arch,
            upcall: Spinlock::new(None),
            slots: Mutex::new(SlotMgr::default()),
        }
    }

    pub fn new_kernel() -> Self {
        Self::__new(ArchContext::new_kernel())
    }

    pub fn new() -> Self {
        Self::__new(ArchContext::new())
    }

    pub(super) fn init_kernel_context(&self) {
        let proto = unsafe { Mapper::current() };
        let rm = proto.readmap(MappingCursor::new(
            VirtAddr::start_kernel_memory(),
            usize::MAX,
        ));
        for map in rm.coalesce() {
            let cursor = MappingCursor::new(map.vaddr(), map.len());
            let mut phys = ContiguousProvider::new(map.paddr(), map.len());
            let settings = MappingSettings::new(
                map.settings().perms(),
                map.settings().cache(),
                map.settings().flags() | MappingFlags::GLOBAL,
            );
            self.arch.map(cursor, &mut phys, &settings);
        }
    }
}

impl UserContext for VirtContext {
    type UpcallInfo = VirtAddr;
    type MappingInfo = Slot;

    fn set_upcall(&self, target: Self::UpcallInfo) {
        *self.upcall.lock() = Some(target);
    }

    fn get_upcall(&self) -> Option<Self::UpcallInfo> {
        *self.upcall.lock()
    }

    fn switch_to(&self) {
        self.arch.switch_to();
    }

    fn insert_object(
        &self,
        slot: Slot,
        object_info: &ObjectContextInfo,
    ) -> Result<(), InsertError> {
        let new_slot_info = VirtContextSlot {
            obj: object_info.object().clone(),
            slot,
            perms: object_info.perms(),
            cache: object_info.cache(),
        };
        let mut slots = self.slots.lock();
        if let Some(info) = slots.get(slot) {
            if info != &new_slot_info {
                return Err(InsertError::Occupied);
            }
            return Ok(());
        }
        slots.insert(slot, object_info.object().id(), new_slot_info);
        Ok(())
    }

    fn remove_object(&self, _obj: twizzler_abi::object::ObjID, _start: usize, _len: usize) {
        todo!()
    }

    fn write_protect(&self, obj: ObjID, start: usize, len: usize) {
        let slots = self.slots.lock();
        if let Some(slot) = slots.obj_to_slot(obj) {
            self.wp_slot(slot, start, len);
        }
    }

    fn lookup_object(&self, info: Self::MappingInfo) -> Option<ObjectContextInfo> {
        todo!()
    }
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct VirtContextSlot {
    obj: ObjectRef,
    slot: Slot,
    perms: MappingPerms,
    cache: CacheType,
}

impl VirtContextSlot {
    fn mapping_cursor(&self, start: usize, len: usize) -> MappingCursor {
        // TODO
        MappingCursor::new(self.slot.start_vaddr().offset(start as isize).unwrap(), len)
    }

    fn mapping_settings(&self, wp: bool) -> MappingSettings {
        let mut perms = self.perms;
        if wp {
            perms.remove(MappingPerms::WRITE);
        }
        MappingSettings::new(perms, self.cache, MappingFlags::empty())
    }

    fn phys_provider(&self) -> ObjectPageProvider {
        ObjectPageProvider {
            obj: self.obj.clone(),
        }
    }
}

// TODO: arch-dep
pub const HEAP_START: u64 = 0xffffff0000000000;
pub const HEAP_MAX_LEN: usize = 0x0000001000000000 / 16; //4GB

struct GlobalPageAlloc {
    alloc: linked_list_allocator::Heap,
    end: VirtAddr,
}

impl GlobalPageAlloc {
    fn extend(&mut self, len: usize, mapper: &VirtContext) {
        let cursor = MappingCursor::new(self.end, len);
        let mut phys = ZeroPageProvider::default();
        let settings = MappingSettings::new(
            MappingPerms::READ | MappingPerms::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        mapper.arch.map(cursor, &mut phys, &settings);
        self.end = self.end.offset(len).unwrap();
        // Safety: the extension is backed by memory that is directly after the previous call to extend.
        unsafe {
            self.alloc.extend(len);
        }
    }

    fn init(&mut self, mapper: &VirtContext) {
        let len = 2 * 1024 * 1024;
        let cursor = MappingCursor::new(self.end, len);
        let mut phys = ZeroPageProvider::default();
        let settings = MappingSettings::new(
            MappingPerms::READ | MappingPerms::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        mapper.arch.map(cursor, &mut phys, &settings);
        self.end = self.end.offset(len).unwrap();
        // Safety: the initial is backed by memory.
        unsafe {
            self.alloc.init(HEAP_START as *mut u8, len);
        }
    }
}

// Safety: the internal heap contains raw pointers, which are not Send. However, the heap is globally mapped and static
// for the lifetime of the kernel.
unsafe impl Send for GlobalPageAlloc {}

static GLOBAL_PAGE_ALLOC: Spinlock<GlobalPageAlloc> = Spinlock::new(GlobalPageAlloc {
    alloc: linked_list_allocator::Heap::empty(),
    end: VirtAddr::new(HEAP_START).ok().unwrap(),
});

impl KernelMemoryContext for VirtContext {
    fn allocate_chunk(&self, layout: core::alloc::Layout) -> NonNull<u8> {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        let res = glb.alloc.allocate_first_fit(layout);
        match res {
            Err(_) => {
                let size = layout
                    .pad_to_align()
                    .size()
                    .next_multiple_of(crate::memory::pagetables::Table::level_to_page_size(0))
                    * 2;
                glb.extend(size, self);
                glb.alloc.allocate_first_fit(layout).unwrap()
            }
            Ok(x) => x,
        }
    }

    unsafe fn deallocate_chunk(&self, layout: core::alloc::Layout, ptr: NonNull<u8>) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        // TODO: reclaim and unmap?
        glb.alloc.deallocate(ptr, layout);
    }

    fn init_allocator(&self) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        glb.init(self);
    }
}
bitflags::bitflags! {
    pub struct PageFaultFlags : u32 {
        const USER = 1;
        const INVALID = 2;
        const PRESENT = 4;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PageFaultCause {
    InstructionFetch,
    Read,
    Write,
}

pub fn page_fault(addr: VirtAddr, cause: PageFaultCause, flags: PageFaultFlags, ip: VirtAddr) {
    if false {
        logln!(
            "(thrd {}) page fault at {:?} cause {:?} flags {:?}, at {:?}",
            current_thread_ref().map(|t| t.id()).unwrap_or(0),
            addr,
            cause,
            flags,
            ip
        );
    }
    /* TODO: null page */
    if !flags.contains(PageFaultFlags::USER) && addr.is_kernel()
    /*TODO */
    {
        panic!(
            "kernel page fault addr={:?} {:?} {:?} ip={:?}",
            addr, cause, flags, ip
        )
    }
    let vmc = current_memory_context();
    if vmc.is_none() {
        panic!("page fault in thread with no memory context");
    }
    let vmc = vmc.unwrap();
    let mapping = { vmc.inner().lookup_object(addr) };

    if let Some(mapping) = mapping {
        let objid = mapping.obj.id();
        let page_number = PageNumber::from_address(addr);
        let mut obj_page_tree = mapping.obj.lock_page_tree();
        let is_write = cause == PageFaultCause::Write;

        if page_number == PageNumber::from_address(VirtAddr::new(0).unwrap()) {
            panic!("zero-page fault {:?} ip: {:?} cause {:?}", addr, ip, cause);
        }

        if let Some((page, cow)) = obj_page_tree.get_page(page_number, is_write) {
            let mut vmc = vmc.inner();
            /* check if mappings changed */
            if vmc.lookup_object(addr).map_or(0.into(), |o| o.obj.id()) != objid {
                drop(vmc);
                drop(obj_page_tree);
                return page_fault(addr, cause, flags, ip);
            }
            //TODO: get these perms from the second lookup
            let perms = if cow {
                mapping.perms & MappingPerms::WRITE.complement()
            } else {
                mapping.perms
            };
            if false {
                logln!(
                    "  => mapping {:?} page {:?} {:?}",
                    objid,
                    page_number,
                    page.physical_address()
                );
            }
            vmc.map_object_page(addr, page, perms);
        } else {
            let page = Page::new();
            obj_page_tree.add_page(page_number, page);
            drop(obj_page_tree);
            page_fault(addr, cause, flags, ip);
        }
    } else {
        //TODO: fault
        if let Some(th) = current_thread_ref() {
            logln!("user fs {:?}", th.arch.user_fs);
        }
        panic!("page fault: no obj");
    }
}
