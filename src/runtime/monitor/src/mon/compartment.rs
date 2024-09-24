use std::collections::HashMap;

use dynlink::{compartment::CompartmentId, library::UnloadedLibrary};
use happylock::ThreadKey;
use secgate::util::Descriptor;
use twizzler_runtime_api::ObjID;

use crate::{
    api::MONITOR_INSTANCE_ID,
    gates::{CompartmentInfo, LoadCompartmentError},
};

mod compconfig;
mod compthread;
mod loader;
mod runcomp;

pub use compconfig::*;
pub use loader::RunCompLoader;
pub use runcomp::*;

/// Manages compartments.
#[derive(Default)]
pub struct CompartmentMgr {
    names: HashMap<String, ObjID>,
    instances: HashMap<ObjID, RunComp>,
    dynlink_map: HashMap<CompartmentId, ObjID>,
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

    /// Get a [RunComp] by name.
    pub fn get_dynlinkid(&self, id: CompartmentId) -> Option<&RunComp> {
        let id = self.dynlink_map.get(&id)?;
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

    /// Get a [RunComp] by name.
    pub fn get_dynlinkid_mut(&mut self, id: CompartmentId) -> Option<&mut RunComp> {
        let id = self.dynlink_map.get(&id)?;
        self.get_mut(*id)
    }

    /// Insert a [RunComp].
    pub fn insert(&mut self, rc: RunComp) {
        self.names.insert(rc.name.clone(), rc.instance);
        self.dynlink_map.insert(rc.compartment_id, rc.instance);
        self.instances.insert(rc.instance, rc);
    }

    /// Remove a [RunComp].
    pub fn remove(&mut self, id: ObjID) -> Option<RunComp> {
        let rc = self.instances.remove(&id)?;
        self.names.remove(&rc.name);
        self.dynlink_map.remove(&rc.compartment_id);
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

    /// Get an iterator over all compartments.
    pub fn compartments(&self) -> impl Iterator<Item = &RunComp> {
        self.instances.values()
    }

    /// Get an iterator over all compartments (mutable).
    pub fn compartments_mut(&mut self) -> impl Iterator<Item = &mut RunComp> {
        self.instances.values_mut()
    }
}

impl super::Monitor {
    /// Get CompartmentInfo for this caller. Note that this will write to the compartment-thread's
    /// simple buffer.
    pub fn get_compartment_info(
        &self,
        instance: ObjID,
        thread: ObjID,
        desc: Option<Descriptor>,
    ) -> Option<CompartmentInfo> {
        let (ref mut space, _, ref mut comps, _, _, ref comphandles) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let comp_id = desc
            .map(|comp| comphandles.lookup(instance, comp).map(|ch| ch.instance))
            .unwrap_or(Some(instance))?;

        let name = comps.get(comp_id)?.name.clone();
        let pt = comps.get_mut(instance)?.get_per_thread(thread, space);
        let name_len = pt.write_bytes(name.as_bytes());
        let comp = comps.get(comp_id)?;

        Some(CompartmentInfo {
            name_len,
            id: comp_id,
            sctx: comp.sctx,
            flags: comp.raw_flags(),
        })
    }

    /// Open a compartment handle for this caller compartment.
    pub fn get_compartment_handle(&self, caller: ObjID, compartment: ObjID) -> Option<Descriptor> {
        self.compartment_handles
            .write(ThreadKey::get().unwrap())
            .insert(
                caller,
                super::CompartmentHandle {
                    instance: if compartment.as_u128() == 0 {
                        caller
                    } else {
                        compartment
                    },
                },
            )
    }

    /// Open a handle to the n'th dependency compartment of a given compartment.
    pub fn get_compartment_deps(
        &self,
        caller: ObjID,
        desc: Option<Descriptor>,
        dep_n: usize,
    ) -> Option<Descriptor> {
        todo!()
    }

    /// Load a new compartment with a root library ID, and return a compartment handle.
    pub fn load_compartment(
        &self,
        caller: ObjID,
        thread: ObjID,
        name_len: usize,
    ) -> Result<Descriptor, LoadCompartmentError> {
        let sctx = caller; //TODO
        let name_bytes = self
            .read_thread_simple_buffer(caller, thread, name_len)
            .ok_or(LoadCompartmentError::Unknown)?;
        let name = String::from_utf8_lossy(&name_bytes);
        let root = UnloadedLibrary::new(name.clone());

        let loader = {
            let mut dynlink = self.dynlink.write(ThreadKey::get().unwrap());

            let loader = loader::RunCompLoader::new(&mut *dynlink, &name, root);
            tracing::info!("loader: {:#?}", loader);
            loader
        }
        .unwrap();

        let (ref mut space, _, ref mut cmp, ref mut dynlink, _, _) =
            &mut *self.locks.lock(ThreadKey::get().unwrap());
        let comps = loader.build_rcs(&mut *cmp, &mut *dynlink, &mut *space);
        tracing::info!("loader2: comps: {:?}", comps);

        for c in comps.unwrap() {
            let rc = cmp.get(c).unwrap();
            tracing::info!("==> {}: {:#?}", c, rc);
        }

        loop {}
        todo!()
    }

    /// Drop a compartment handle.
    pub fn drop_compartment_handle(&self, caller: ObjID, desc: Descriptor) {
        self.compartment_handles
            .write(ThreadKey::get().unwrap())
            .remove(caller, desc);
    }
}

/// A handle to a compartment.
pub struct CompartmentHandle {
    pub instance: ObjID,
}
