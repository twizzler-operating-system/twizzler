use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use dynlink::engines::Engine;
use twizzler_runtime_api::{MapError, MapFlags, ObjID};

use crate::mapman::{MapInfo, MappedObjectAddrs};

use self::runcomp::{RunComp, RunCompInner};

mod object;
mod runcomp;
mod thread;

pub(crate) struct CompMan {
    inner: Mutex<CompManInner>,
    dynlink: Mutex<Option<dynlink::context::Context<Engine>>>,
}

lazy_static::lazy_static! {
pub(crate) static ref COMPMAN: CompMan = CompMan::new();
}

impl CompMan {
    fn new() -> Self {
        Self {
            inner: Mutex::new(CompManInner::default()),
            dynlink: Mutex::new(None),
        }
    }
}

#[derive(Default)]
struct CompManInner {
    name_map: HashMap<String, ObjID>,
    instance_map: HashMap<ObjID, RunComp>,
}

impl CompManInner {
    pub fn insert(&mut self, rc: RunComp) {
        self.name_map.insert(rc.name().to_string(), rc.instance);
        self.instance_map.insert(rc.instance, rc);
    }

    pub fn lookup(&mut self, instance: ObjID) -> Option<&RunComp> {
        self.instance_map.get(&instance)
    }

    pub fn lookup_name(&mut self, name: &str) -> Option<&RunComp> {
        self.lookup(*self.name_map.get(name)?)
    }

    pub fn lookup_instance(&mut self, name: &str) -> Option<ObjID> {
        self.name_map.get(name).cloned()
    }

    pub fn remove(&mut self, instance: ObjID) -> Option<RunComp> {
        let Some(rc) = self.instance_map.remove(&instance) else {
            return None;
        };
        self.name_map.remove(rc.name());
        Some(rc)
    }
}

impl CompMan {
    fn get_comp_inner(&self, comp_id: ObjID) -> Option<Arc<Mutex<RunCompInner>>> {
        // Lock, get inner and clone, and release lock. Consumers of this function can then safely lock the inner RC without
        // holding the CompMan lock.
        let mut inner = self.inner.lock().ok()?;
        let rc = inner.lookup(comp_id)?;
        Some(rc.cloned_inner())
    }

    pub fn map_object(
        &self,
        comp_id: ObjID,
        id: ObjID,
        flags: MapFlags,
    ) -> Result<MappedObjectAddrs, MapError> {
        let rc = self
            .get_comp_inner(comp_id)
            .ok_or(MapError::InternalError)?;
        let mut rc = rc.lock().map_err(|_| MapError::InternalError)?;
        rc.map_object(MapInfo { id, flags })
    }

    pub fn unmap_object(&self, comp_id: ObjID, id: ObjID, flags: MapFlags) -> Result<(), MapError> {
        let rc = self
            .get_comp_inner(comp_id)
            .ok_or(MapError::InternalError)?;
        let mut rc = rc.lock().map_err(|_| MapError::InternalError)?;
        rc.unmap_object(MapInfo { id, flags });
        Ok(())
    }
}
