#![feature(naked_functions)]

use std::{collections::BTreeMap, sync::Mutex, time::Instant};

use twizzler_abi::{
    object::ObjID,
    syscall::{sys_object_ctrl, ObjectControlCmd},
};
use twizzler_rt_abi::{
    object::{MapFlags, ObjectHandle},
    Result,
};

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CachedStats {
    pub id: ObjID,
    pub start: Instant,
    pub flags: MapFlags,
    pub addr: u64,
}

type Key = (ObjID, MapFlags);

struct HeldObject {
    start: Instant,
    handle: ObjectHandle,
}

impl HeldObject {
    fn new(handle: ObjectHandle) -> Self {
        Self {
            start: Instant::now(),
            handle,
        }
    }
}

struct CacheState {
    map: BTreeMap<Key, HeldObject>,
}

impl CacheState {
    const fn new() -> Self {
        Self {
            map: BTreeMap::new(),
        }
    }
}

static STATE: Mutex<CacheState> = Mutex::new(CacheState::new());

#[secgate::secure_gate]
pub fn hold(id: ObjID, flags: MapFlags) -> Result<bool> {
    let handle = twizzler_rt_abi::object::twz_rt_map_object(id, flags)?;
    let mut state = STATE.lock().unwrap();
    Ok(state
        .map
        .insert((id, flags), HeldObject::new(handle))
        .is_some())
}

#[secgate::secure_gate]
pub fn drop(id: ObjID, flags: MapFlags) -> Result<bool> {
    let mut state = STATE.lock().unwrap();
    Ok(state.map.remove(&(id, flags)).is_some())
}

#[secgate::secure_gate]
pub fn preload(id: ObjID) -> Result<()> {
    sys_object_ctrl(id, ObjectControlCmd::Preload)
}

#[secgate::secure_gate]
pub fn stat(_id: ObjID) -> Result<()> {
    Ok(())
}

#[secgate::secure_gate]
pub fn list_nth(nth: u64) -> Result<Option<CachedStats>> {
    let state = STATE.lock().unwrap();
    if let Some(v) = state.map.values().nth(nth as usize) {
        Ok(Some(CachedStats {
            id: v.handle.id(),
            flags: v.handle.map_flags(),
            start: v.start,
            addr: v.handle.start().addr() as u64,
        }))
    } else {
        Ok(None)
    }
}
