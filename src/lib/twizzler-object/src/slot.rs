use std::{
    collections::HashMap,
    mem::size_of,
    sync::{Arc, Mutex, Weak},
};

use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::MapFlags,
};

use crate::{meta::FotEntry, ObjectInitError};

/// A slot for an object in active memory. All unique combinations of an object ID and Protections
/// are given a slot. The exact slot locations may be reused. Typically, slots are reference
/// counted, and when dropped, release the slot for reuse. The object may or may not be unmapped
/// immediately following the slot's drop.
pub struct Slot {
    n: usize,
    id: ObjID,
    prot: Protections,
}

impl Slot {
    fn new(id: ObjID, prot: Protections) -> Result<Self, ObjectInitError> {
        let n = twizzler_abi::slot::global_allocate().ok_or(ObjectInitError::OutOfSlots)?;
        let _result = twizzler_abi::syscall::sys_object_map(None, id, n, prot, MapFlags::empty())
            .map_err::<ObjectInitError, _>(|e| e.into())?;
        Ok(Self { n, id, prot })
    }

    /// Get the ID of the object in this slot.
    pub fn id(&self) -> ObjID {
        self.id
    }

    /// Get the slot number of this slot.
    pub fn slot(&self) -> usize {
        self.n
    }

    /// Get the protections of this slot.
    pub fn prot(&self) -> Protections {
        self.prot
    }

    /// Get the vaddr of this slot's object base.
    pub fn vaddr_start(&self) -> usize {
        twizzler_abi::slot::to_vaddr_range(self.n).0
    }

    /// Get the vaddr of this slot's object's null page.
    pub fn vaddr_null(&self) -> usize {
        twizzler_abi::slot::to_vaddr_range(self.n).0 - twizzler_abi::object::NULLPAGE_SIZE
    }

    /// Get the vaddr of this slot's object's meta page.
    pub fn vaddr_meta(&self) -> usize {
        twizzler_abi::slot::to_vaddr_range(self.n).1
    }

    /// Perform a raw load-effective-address for an offset into a slot.
    pub fn raw_lea<P>(&self, off: usize) -> *const P {
        let start = self.vaddr_null();
        unsafe { ((start + off) as *const P).as_ref().unwrap() }
    }

    /// Perform a raw load-effective-address for an offset into a slot.
    pub fn raw_lea_mut<P>(&self, off: usize) -> *mut P {
        let start = self.vaddr_null();
        unsafe { ((start + off) as *mut P).as_mut().unwrap() }
    }

    /// Get a mutable pointer to one of the slot's object's FOT entries.
    ///
    /// # Safety
    /// See this crate's base documentation ([Isolation Safety](crate)).
    pub unsafe fn get_fote_unguarded(&self, idx: usize) -> *mut FotEntry {
        let end = self.vaddr_meta();
        let off = idx * size_of::<FotEntry>();
        ((end - off) + twizzler_abi::object::NULLPAGE_SIZE / 2) as *mut FotEntry
    }
}

lazy_static::lazy_static! {
static ref SLOTS: Mutex<HashMap<(ObjID, Protections), Weak<Slot>>> = Mutex::new(HashMap::new());
}

/// Get a slot for an object and protections combo.
pub fn get(id: ObjID, prot: Protections) -> Result<Arc<Slot>, ObjectInitError> {
    let mut slots = SLOTS.lock().unwrap();
    if let Some(slot) = slots.get(&(id, prot)) {
        if let Some(slot) = slot.clone().upgrade() {
            return Ok(slot);
        } else {
            slots.remove(&(id, prot));
        }
    }
    let slot = Arc::new(Slot::new(id, prot)?);
    let w = Arc::downgrade(&slot);
    slots.insert((id, prot), w);
    Ok(slot)
}

impl Drop for Slot {
    fn drop(&mut self) {
        twizzler_abi::slot::global_release(self.n);
    }
}
