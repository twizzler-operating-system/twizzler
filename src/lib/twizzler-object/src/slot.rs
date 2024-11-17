use std::{
    collections::HashMap,
    mem::size_of,
    sync::{Arc, Mutex, Weak},
};

use twizzler_abi::object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::object::{MapError, MapFlags, ObjectHandle};

use crate::{meta::FotEntry, ObjectInitError};

/// A slot for an object in active memory. All unique combinations of an object ID and Protections
/// are given a slot. The exact slot locations may be reused. Typically, slots are reference
/// counted, and when dropped, release the slot for reuse. The object may or may not be unmapped
/// immediately following the slot's drop.
pub struct Slot {
    id: ObjID,
    prot: Protections,
    runtime_handle: ObjectHandle,
}

unsafe impl Sync for Slot {}
unsafe impl Send for Slot {}

impl From<MapError> for ObjectInitError {
    fn from(me: MapError) -> Self {
        match me {
            MapError::OutOfResources => ObjectInitError::OutOfSlots,
            MapError::NoSuchObject => ObjectInitError::ObjectNotFound,
            _ => ObjectInitError::MappingFailed,
        }
    }
}

fn into_map_flags(p: Protections) -> MapFlags {
    let mut flags = MapFlags::empty();
    if p.contains(Protections::EXEC) {
        flags.insert(MapFlags::EXEC);
    }

    if p.contains(Protections::READ) {
        flags.insert(MapFlags::READ);
    }

    if p.contains(Protections::WRITE) {
        flags.insert(MapFlags::WRITE);
    }
    flags
}

fn into_protections(flags: MapFlags) -> Protections {
    let mut prot = Protections::empty();
    if flags.contains(MapFlags::EXEC) {
        prot.insert(Protections::EXEC);
    }

    if flags.contains(MapFlags::READ) {
        prot.insert(Protections::READ);
    }

    if flags.contains(MapFlags::WRITE) {
        prot.insert(Protections::WRITE);
    }
    prot
}

impl Slot {
    fn new(id: ObjID, prot: Protections) -> Result<Self, ObjectInitError> {
        let rh = twizzler_rt_abi::object::twz_rt_map_object(id, into_map_flags(prot))?;
        Ok(Self {
            id,
            prot,
            runtime_handle: rh,
        })
    }

    pub fn new_from_handle(handle: ObjectHandle) -> Result<Self, ObjectInitError> {
        Ok(Self {
            id: handle.id(),
            prot: into_protections(handle.map_flags()),
            runtime_handle: handle,
        })
    }

    pub fn runtime_handle(&self) -> &ObjectHandle {
        &self.runtime_handle
    }

    pub fn slot_number(&self) -> usize {
        self.vaddr_null() / MAX_SIZE
    }

    /// Get the ID of the object in this slot.
    pub fn id(&self) -> ObjID {
        self.id
    }

    /// Get the protections of this slot.
    pub fn prot(&self) -> Protections {
        self.prot
    }

    /// Get the vaddr of this slot's object base.
    pub fn vaddr_base(&self) -> usize {
        self.vaddr_null() + NULLPAGE_SIZE
    }

    /// Get the vaddr of this slot's object's null page.
    pub fn vaddr_null(&self) -> usize {
        self.runtime_handle.start() as usize
        //self.runtime_handle.base.expose_addr()
    }

    /// Get the vaddr of this slot's object's meta page.
    pub fn vaddr_meta(&self) -> usize {
        self.vaddr_null() + MAX_SIZE - NULLPAGE_SIZE
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
