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

    pub fn id(&self) -> ObjID {
        self.id
    }

    pub fn slot(&self) -> usize {
        self.n
    }

    pub fn prot(&self) -> Protections {
        self.prot
    }

    pub fn vaddr_start(&self) -> usize {
        twizzler_abi::slot::to_vaddr_range(self.n).0
    }

    pub fn vaddr_null(&self) -> usize {
        twizzler_abi::slot::to_vaddr_range(self.n).0 - twizzler_abi::object::NULLPAGE_SIZE
    }

    pub fn vaddr_meta(&self) -> usize {
        twizzler_abi::slot::to_vaddr_range(self.n).1
    }

    pub fn raw_lea<P>(&self, off: usize) -> *const P {
        let start = self.vaddr_start();
        unsafe { ((start + off) as *const P).as_ref().unwrap() }
    }

    pub fn raw_lea_mut<P>(&self, off: usize) -> *mut P {
        let start = self.vaddr_start();
        unsafe { ((start + off) as *mut P).as_mut().unwrap() }
    }

    pub unsafe fn get_fote_unchecked(&self, idx: usize) -> &FotEntry {
        let end = self.vaddr_meta();
        let off = idx * size_of::<FotEntry>();
        (((end - off) + twizzler_abi::object::NULLPAGE_SIZE / 2) as *const FotEntry)
            .as_ref()
            .unwrap()
    }
}

lazy_static::lazy_static! {
static ref SLOTS: Mutex<HashMap<(ObjID, Protections), Weak<Slot>>> = Mutex::new(HashMap::new());
}

pub fn get(id: ObjID, prot: Protections) -> Result<Arc<Slot>, ObjectInitError> {
    let mut slots = SLOTS.lock().unwrap();
    if let Some(slot) = slots.get(&(id, prot)) {
        if let Some(slot) = slot.clone().upgrade() {
            return Ok(slot);
        } else {
            drop(slot);
            slots.remove(&(id, prot));
        }
    }
    let slot = Arc::new(Slot::new(id, prot)?);
    let w = Arc::downgrade(&slot.clone());
    slots.insert((id, prot), w);
    Ok(slot)
}

impl Drop for Slot {
    fn drop(&mut self) {
        twizzler_abi::slot::global_release(self.n);
    }
}
