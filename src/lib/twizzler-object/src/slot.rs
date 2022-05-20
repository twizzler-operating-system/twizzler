use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};

use twizzler_abi::object::{ObjID, Protections};

struct Slot {
    n: usize,
    id: ObjID,
    prot: Protections,
}

impl Slot {
    fn new(id: ObjID, prot: Protections) -> Self {
        Self {
            n: twizzler_abi::slot::global_allocate().unwrap(),
            id,
            prot,
        }
    }
}

lazy_static::lazy_static! {
static ref SLOTS: Mutex<HashMap<(ObjID, Protections), Weak<Slot>>> = Mutex::new(HashMap::new());
}

fn get(id: ObjID, prot: Protections) -> Arc<Slot> {
    let mut slots = SLOTS.lock().unwrap();
    if let Some(slot) = slots.get(&(id, prot)) {
        if let Some(slot) = slot.clone().upgrade() {
            return slot;
        } else {
            drop(slot);
            slots.remove(&(id, prot));
        }
    }
    let slot = Arc::new(Slot::new(id, prot));
    let w = Arc::downgrade(&slot.clone());
    slots.insert((id, prot), w);
    slot
}

impl Drop for Slot {
    fn drop(&mut self) {
        twizzler_abi::slot::global_release(self.n);
    }
}
