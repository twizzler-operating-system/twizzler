use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

use dynlink::{
    compartment::Compartment,
    context::{engine::ContextEngine, Context},
    engines::Engine,
};
use twizzler_runtime_api::{MapError, MapFlags, ObjID};

use crate::{
    api::MONITOR_INSTANCE_ID,
    init::InitDynlinkContext,
    mapman::{MapHandle, MapInfo},
};

use self::runcomp::{RunComp, RunCompInner};

mod object;
mod runcomp;
mod stack_object;
mod thread;

pub(crate) struct CompMan {
    inner: Mutex<CompManInner>,
}

lazy_static::lazy_static! {
pub(crate) static ref COMPMAN: CompMan = CompMan::new();
}

impl CompMan {
    fn new() -> Self {
        Self {
            inner: Mutex::new(CompManInner::default()),
        }
    }
}

#[derive(Default)]
pub(crate) struct CompManInner {
    name_map: HashMap<String, ObjID>,
    instance_map: HashMap<ObjID, RunComp>,
    dynlink_state: Option<&'static mut Context<Engine>>,
}

impl CompManInner {
    pub fn dynlink(&self) -> &Context<Engine> {
        self.dynlink_state.as_ref().unwrap()
    }

    pub fn dynlink_mut(&mut self) -> &mut Context<Engine> {
        self.dynlink_state.as_mut().unwrap()
    }

    pub fn get_monitor_dynlink_compartment(
        &mut self,
    ) -> &mut Compartment<<Engine as ContextEngine>::Backing> {
        let id = self.lookup(MONITOR_INSTANCE_ID).unwrap().compartment_id;
        self.dynlink_mut().get_compartment_mut(id).unwrap()
    }

    pub fn insert(&mut self, rc: RunComp) {
        self.name_map.insert(rc.name().to_string(), rc.instance);
        self.instance_map.insert(rc.instance, rc);
    }

    pub fn lookup(&self, instance: ObjID) -> Option<&RunComp> {
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
    pub fn init(&self, mut idc: InitDynlinkContext) {
        let mut cm = self.inner.lock().unwrap();
        cm.dynlink_state = Some(idc.ctx());
    }

    pub fn lock(&self) -> MutexGuard<'_, CompManInner> {
        self.inner.lock().unwrap()
    }

    pub fn with_monitor_compartment<R>(&self, f: impl FnOnce(&RunComp) -> R) -> R {
        let inner = self.inner.lock().unwrap();
        let rc = inner.lookup(MONITOR_INSTANCE_ID).unwrap();
        f(rc)
    }

    pub fn get_comp_inner(&self, comp_id: ObjID) -> Option<Arc<Mutex<RunCompInner>>> {
        // Lock, get inner and clone, and release lock. Consumers of this function can then safely lock the inner RC without
        // holding the CompMan lock.
        let inner = self.inner.lock().ok()?;
        let rc = inner.lookup(comp_id)?;
        Some(rc.cloned_inner())
    }

    pub fn map_object(
        &self,
        comp_id: ObjID,
        id: ObjID,
        flags: MapFlags,
    ) -> Result<MapHandle, MapError> {
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
