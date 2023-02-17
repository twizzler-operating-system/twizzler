use core::ptr::NonNull;

use alloc::collections::BTreeMap;
use twizzler_abi::object::{ObjID, MAX_SIZE};

use super::{Context, InsertError, KernelMemoryContext, MappingPerms};
use crate::{
    arch::{address::VirtAddr, context::ArchContext},
    memory::{
        map::CacheType,
        pagetables::{
            MappingCursor, MappingFlags, MappingSettings, PhysAddrProvider, ZeroPageProvider,
        },
    },
    mutex::Mutex,
    obj::ObjectRef,
    spinlock::Spinlock,
};

/// A type that implements [Context] for virtual memory systems.
pub struct VirtContext {
    arch: ArchContext,
    upcall: Spinlock<Option<VirtAddr>>,
    slots: Mutex<SlotMgr>,
}

#[derive(Default)]
struct SlotMgr {
    slots: BTreeMap<usize, VirtContextSlot>,
    objs: BTreeMap<ObjID, usize>,
}

impl SlotMgr {
    fn get(&self, slot: usize) -> Option<&VirtContextSlot> {
        self.slots.get(&slot)
    }

    fn insert(&mut self, slot: usize, id: ObjID, info: VirtContextSlot) {
        self.slots.insert(slot, info);
        self.objs.insert(id, slot);
    }

    fn remove(&mut self, slot: usize) {
        if let Some(info) = self.slots.remove(&slot) {
            self.objs.remove(&info.obj.id());
        }
    }

    fn obj_to_slot(&self, id: ObjID) -> Option<usize> {
        self.objs.get(&id).cloned()
    }
}

fn slot_to_vaddr(slot: usize) -> VirtAddr {
    // TODO
    VirtAddr::new((slot * MAX_SIZE) as u64).unwrap()
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
    fn map_slot(&self, slot: usize, start: usize, len: usize) {
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

    fn wp_slot(&self, slot: usize, start: usize, len: usize) {
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
}

impl Context for VirtContext {
    type UpcallInfo = VirtAddr;
    type MappingInfo = usize;

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
        obj: ObjectRef,
        slot: usize,
        perms: MappingPerms,
        cache: CacheType,
    ) -> Result<(), InsertError> {
        let new_slot_info = VirtContextSlot {
            obj: obj.clone(),
            slot,
            perms,
            cache,
        };
        let mut slots = self.slots.lock();
        if let Some(info) = slots.get(slot) {
            if info != &new_slot_info {
                return Err(InsertError::Occupied);
            }
            return Ok(());
        }
        slots.insert(slot, obj.id(), new_slot_info);
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
}

#[derive(Eq, PartialEq, Ord, PartialOrd)]
struct VirtContextSlot {
    obj: ObjectRef,
    slot: usize,
    perms: MappingPerms,
    cache: CacheType,
}

impl VirtContextSlot {
    fn mapping_cursor(&self, start: usize, len: usize) -> MappingCursor {
        // TODO
        MappingCursor::new(
            slot_to_vaddr(self.slot).offset(start as isize).unwrap(),
            len,
        )
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

struct GlobalPageAlloc {
    alloc: linked_list_allocator::Heap,
    end: VirtAddr,
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
        let size = layout
            .size()
            .next_multiple_of(crate::memory::pagetables::Table::level_to_page_size(0));
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        let res = glb.alloc.allocate_first_fit(layout);
        match res {
            Err(_) => {
                let cursor = MappingCursor::new(glb.end, size);
                let mut phys = ZeroPageProvider::default();
                let settings = MappingSettings::new(
                    MappingPerms::READ | MappingPerms::WRITE,
                    CacheType::WriteBack,
                    MappingFlags::GLOBAL,
                );
                self.arch.map(cursor, &mut phys, &settings);
                glb.end = glb.end.offset(size).unwrap();
                // Safety: the extension is backed by memory that is directly after the previous call to extend.
                unsafe {
                    glb.alloc.extend(size);
                }
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
}
