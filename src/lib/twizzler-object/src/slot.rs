use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock, Weak},
};

use twizzler_abi::{
    object::{ObjID, Protections},
    syscall::MapFlags,
};

use crate::ObjectInitError;

pub(crate) struct Slot {
    n: usize,
    id: ObjID,
    prot: Protections,
    fot_cache: RwLock<HashMap<usize, FotCacheEntry>>,
}

impl Slot {
    fn new(id: ObjID, prot: Protections) -> Result<Self, ObjectInitError> {
        let n = twizzler_abi::slot::global_allocate().ok_or(ObjectInitError::OutOfSlots)?;
        let _result = twizzler_abi::syscall::sys_object_map(None, id, n, prot, MapFlags::empty())
            .map_err::<ObjectInitError, _>(|e| e.into())?;
        Ok(Self {
            n,
            id,
            prot,
            fot_cache: RwLock::new(HashMap::new()),
        })
    }

    pub fn id(&self) -> ObjID {
        self.id
    }

    pub fn slot(&self) -> usize {
        self.n
    }

    pub fn vaddr_start(&self) -> usize {
        twizzler_abi::slot::to_vaddr_range(self.n).0
    }

    pub fn vaddr_meta(&self) -> usize {
        twizzler_abi::slot::to_vaddr_range(self.n).1
    }
}

lazy_static::lazy_static! {
static ref SLOTS: Mutex<HashMap<(ObjID, Protections), Weak<Slot>>> = Mutex::new(HashMap::new());
}

pub(crate) fn get(id: ObjID, prot: Protections) -> Result<Arc<Slot>, ObjectInitError> {
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

struct FotCacheEntry {
    target: Arc<Slot>,
}
