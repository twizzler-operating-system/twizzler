use std::collections::HashMap;

use secgate::util::Descriptor;
use twizzler_runtime_api::ObjID;

use crate::{
    api::MONITOR_INSTANCE_ID,
    gates::{CompartmentInfo, LoadCompartmentError},
};

mod compconfig;
mod compthread;
mod runcomp;

pub use compconfig::*;
pub use runcomp::*;

/// Manages compartments.
#[derive(Default)]
pub struct CompartmentMgr {
    names: HashMap<String, ObjID>,
    instances: HashMap<ObjID, RunComp>,
}

impl CompartmentMgr {
    /// Get a [RunComp] by instance ID.
    pub fn get(&self, id: ObjID) -> Option<&RunComp> {
        self.instances.get(&id)
    }

    /// Get a [RunComp] by name.
    pub fn get_name(&self, name: &str) -> Option<&RunComp> {
        let id = self.names.get(name)?;
        self.get(*id)
    }

    /// Get a [RunComp] by instance ID.
    pub fn get_mut(&mut self, id: ObjID) -> Option<&mut RunComp> {
        self.instances.get_mut(&id)
    }

    /// Get a [RunComp] by name.
    pub fn get_name_mut(&mut self, name: &str) -> Option<&mut RunComp> {
        let id = self.names.get(name)?;
        self.get_mut(*id)
    }

    /// Insert a [RunComp].
    pub fn insert(&mut self, rc: RunComp) {
        self.names.insert(rc.name.clone(), rc.instance);
        self.instances.insert(rc.instance, rc);
    }

    /// Remove a [RunComp].
    pub fn remove(&mut self, id: ObjID) -> Option<RunComp> {
        let rc = self.instances.remove(&id)?;
        self.names.remove(&rc.name);
        Some(rc)
    }

    /// Get the [RunComp] for the monitor.
    pub fn get_monitor(&self) -> &RunComp {
        // Unwrap-Ok: this instance is always present.
        self.get(MONITOR_INSTANCE_ID).unwrap()
    }

    /// Get the [RunComp] for the monitor.
    pub fn get_monitor_mut(&mut self) -> &mut RunComp {
        // Unwrap-Ok: this instance is always present.
        self.get_mut(MONITOR_INSTANCE_ID).unwrap()
    }

    pub fn compartments(&self) -> impl Iterator<Item = &RunComp> {
        self.instances.values()
    }

    pub fn compartments_mut(&mut self) -> impl Iterator<Item = &mut RunComp> {
        self.instances.values_mut()
    }
}

impl super::Monitor {
    pub fn get_compartment_info(&self, caller: ObjID, desc: Option<Descriptor>) -> CompartmentInfo {
        todo!()
    }

    pub fn get_compartment_handle(&self, caller: ObjID, compartment: ObjID) -> Option<Descriptor> {
        todo!()
    }

    pub fn get_compartment_deps(
        &self,
        caller: ObjID,
        desc: Option<Descriptor>,
        dep_n: usize,
    ) -> Option<Descriptor> {
        todo!()
    }

    pub fn load_compartment(
        &self,
        caller: ObjID,
        root_id: ObjID,
    ) -> Result<Descriptor, LoadCompartmentError> {
        todo!()
    }

    pub fn drop_compartment_handle(&self, caller: ObjID, desc: Descriptor) {
        todo!()
    }
}
