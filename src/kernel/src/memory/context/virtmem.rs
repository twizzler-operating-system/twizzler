use alloc::collections::BTreeMap;
use twizzler_abi::object::{ObjID, MAX_SIZE};

use super::{Context, InsertError, MappingPerms};
use crate::{
    arch::{address::VirtAddr, context::ArchContext},
    memory::{
        map::CacheType,
        pagetables::{MappingCursor, MappingFlags, MappingSettings, PhysAddrProvider},
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
