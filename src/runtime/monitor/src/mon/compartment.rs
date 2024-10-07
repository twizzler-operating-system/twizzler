use std::{borrow::BorrowMut, collections::HashMap};

use dynlink::{
    compartment::CompartmentId,
    context::{Context, NewCompartmentFlags},
    library::UnloadedLibrary,
};
use happylock::ThreadKey;
use secgate::util::Descriptor;
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync, ThreadSyncSleep};
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
pub(crate) use compthread::StackObject;
pub use loader::RunCompLoader;
pub use runcomp::*;

use super::stat::CompartmentMgrStats;

/// Manages compartments.
#[derive(Default)]
pub struct CompartmentMgr {
    names: HashMap<String, ObjID>,
    instances: HashMap<ObjID, RunComp>,
    dynlink_map: HashMap<CompartmentId, ObjID>,
    cleanup_queue: Vec<RunComp>,
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
    pub fn insert(&mut self, mut rc: RunComp) {
        if self.names.contains_key(&rc.name) {
            // TODO
            rc.name = format!("{}-", rc.name);
            return self.insert(rc);
        }
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

    fn update_compartment_flags(
        &mut self,
        instance: ObjID,
        f: impl FnOnce(u64) -> Option<u64>,
    ) -> bool {
        let Some(rc) = self.get_mut(instance) else {
            return false;
        };

        let flags = rc.raw_flags();
        let Some(new_flags) = f(flags) else {
            return false;
        };
        if flags == new_flags {
            return true;
        }

        rc.cas_flag(flags, new_flags).is_ok()
    }

    fn load_compartment_flags(&self, instance: ObjID) -> u64 {
        let Some(rc) = self.get(instance) else {
            return 0;
        };
        rc.raw_flags()
    }

    fn wait_for_compartment_state_change(
        &self,
        instance: ObjID,
        state: u64,
    ) -> Option<ThreadSyncSleep> {
        let Some(rc) = self.get(instance) else {
            return None;
        };
        Some(rc.until_change(state))
    }

    pub fn main_thread_exited(&mut self, instance: ObjID) {
        tracing::debug!("main thread for compartment {} exited", instance);
        while !self.update_compartment_flags(instance, |old| Some(old | COMP_EXITED)) {}

        let Some(rc) = self.get(instance) else {
            tracing::warn!("failed to find compartment {} during exit", instance);
            return;
        };
        for dep in rc.deps.clone() {
            self.dec_use_count(dep);
        }

        let Some(rc) = self.get_mut(instance) else {
            tracing::warn!("failed to find compartment {} during exit", instance);
            return;
        };
        if rc.use_count == 0 {
            if let Some(rc) = self.remove(instance) {
                self.cleanup_queue.push(rc)
            }
        }
    }

    pub fn dec_use_count(&mut self, instance: ObjID) {
        let Some(rc) = self.get_mut(instance) else {
            return;
        };

        let z = rc.dec_use_count();
        let ex = rc.has_flag(COMP_EXITED);
        if z && ex {
            if let Some(rc) = self.remove(instance) {
                self.cleanup_queue.push(rc)
            }
        }
    }

    pub fn stat(&self) -> CompartmentMgrStats {
        CompartmentMgrStats {
            nr_compartments: self.instances.len(),
        }
    }

    pub fn process_cleanup_queue(&mut self, dynlink: &mut Context) {
        for rc in self.cleanup_queue.drain(..) {
            dynlink.unload_compartment(rc.compartment_id);
            /*
            let Ok(dc) = dynlink.get_compartment_mut(rc.compartment_id) else {
                continue;
            };
            let ids = dc.library_ids().collect::<Vec<_>>();
            for id in ids {
                tracing::info!("dynlink: remove id: {:?}", id);
            }
            tracing::info!("dynlink: remove comp: {:?}", rc.compartment_id);
            */
        }
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
        let (_, _, ref mut comps, _, _, ref mut ch) = *self.locks.lock(ThreadKey::get().unwrap());
        let comp = comps.get_mut(compartment)?;
        comp.inc_use_count();
        ch.insert(
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
        new_comp_flags: NewCompartmentFlags,
    ) -> Result<Descriptor, LoadCompartmentError> {
        let sctx = caller; //TODO
        let name_bytes = self
            .read_thread_simple_buffer(caller, thread, name_len)
            .ok_or(LoadCompartmentError::Unknown)?;
        let input = String::from_utf8_lossy(&name_bytes);
        let mut split = input.split("::");
        let compname = split.next().ok_or(LoadCompartmentError::Unknown)?;
        let libname = split.next().ok_or(LoadCompartmentError::Unknown)?;
        let root = UnloadedLibrary::new(libname);

        let loader = {
            let mut dynlink = self.dynlink.write(ThreadKey::get().unwrap());

            let loader = loader::RunCompLoader::new(&mut *dynlink, &compname, root, new_comp_flags);
            loader
        }
        .inspect_err(|e| tracing::debug!("failed to load {}::{}: {}", compname, libname, e))
        .map_err(|e| LoadCompartmentError::Unknown)?;

        let root_comp = {
            let (ref mut space, _, ref mut cmp, ref mut dynlink, _, _) =
                &mut *self.locks.lock(ThreadKey::get().unwrap());
            loader
                .build_rcs(&mut *cmp, &mut *dynlink, &mut *space)
                .inspect_err(|e| tracing::debug!("failed to load {}::{}: {}", compname, libname, e))
                .map_err(|_| LoadCompartmentError::Unknown)?
        };

        let desc = self
            .get_compartment_handle(caller, root_comp)
            .ok_or(LoadCompartmentError::Unknown)?;

        self.start_compartment(root_comp)?;

        Ok(desc)
    }

    /// Drop a compartment handle.
    pub fn drop_compartment_handle(&self, caller: ObjID, desc: Descriptor) {
        let (_, _, ref mut cmgr, ref mut dynlink, _, ref mut comp_handles) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let comp = comp_handles.remove(caller, desc);

        if let Some(comp) = comp {
            cmgr.dec_use_count(comp.instance);
        }
        cmgr.process_cleanup_queue(&mut *dynlink);
    }

    pub fn update_compartment_flags(
        &self,
        instance: ObjID,
        f: impl FnOnce(u64) -> Option<u64>,
    ) -> bool {
        let mut cmp = self.comp_mgr.write(ThreadKey::get().unwrap());
        cmp.update_compartment_flags(instance, f)
    }

    pub fn load_compartment_flags(&self, instance: ObjID) -> u64 {
        let cmp = self.comp_mgr.read(ThreadKey::get().unwrap());
        cmp.load_compartment_flags(instance)
    }

    pub fn wait_for_compartment_state_change(&self, instance: ObjID, state: u64) {
        let cmp = self.comp_mgr.read(ThreadKey::get().unwrap());
        let Some(sl) = cmp.wait_for_compartment_state_change(instance, state) else {
            return;
        };

        if sl.ready() {
            return;
        }
        drop(cmp);

        let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(sl)], None);
    }
}

/// A handle to a compartment.
pub struct CompartmentHandle {
    pub instance: ObjID,
}
