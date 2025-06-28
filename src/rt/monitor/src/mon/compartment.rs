use std::{collections::HashMap, ffi::CStr};

use dynlink::{
    compartment::{Compartment, CompartmentId},
    context::{Context, LoadedOrUnloaded, NewCompartmentFlags},
    library::UnloadedLibrary,
};
use happylock::ThreadKey;
use monitor_api::MONITOR_INSTANCE_ID;
use secgate::util::Descriptor;
use twizzler_abi::syscall::{sys_thread_sync, ThreadSync, ThreadSyncSleep};
use twizzler_rt_abi::{
    error::{ArgumentError, GenericError, NamingError, ResourceError, TwzError},
    object::ObjID,
};

use crate::gates::{CompartmentInfo, CompartmentMgrStats};

mod compconfig;
mod compthread;
mod loader;
mod runcomp;

pub use compconfig::*;
pub(crate) use compthread::StackObject;
pub use runcomp::*;

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
    pub fn get(&self, id: ObjID) -> Result<&RunComp, TwzError> {
        self.instances.get(&id).ok_or(TwzError::INVALID_ARGUMENT)
    }

    /// Get a [RunComp] by name.
    pub fn _get_name(&self, name: &str) -> Result<&RunComp, TwzError> {
        let id = self.names.get(name).ok_or(TwzError::INVALID_ARGUMENT)?;
        self.get(*id)
    }

    /// Get a [RunComp] by instance ID.
    pub fn get_mut(&mut self, id: ObjID) -> Result<&mut RunComp, TwzError> {
        self.instances
            .get_mut(&id)
            .ok_or(TwzError::INVALID_ARGUMENT)
    }

    /// Get a [RunComp] by name.
    pub fn get_name_mut(&mut self, name: &str) -> Result<&mut RunComp, TwzError> {
        let id = self.names.get(name).ok_or(TwzError::INVALID_ARGUMENT)?;
        self.get_mut(*id)
    }

    /// Get a [RunComp] by dynamic linker ID.
    pub fn get_dynlinkid(&self, id: CompartmentId) -> Result<&RunComp, TwzError> {
        let id = self
            .dynlink_map
            .get(&id)
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        self.get(*id)
    }

    /// Get a [RunComp] by dynamic linker ID.
    pub fn _get_dynlinkid_mut(&mut self, id: CompartmentId) -> Result<&mut RunComp, TwzError> {
        let id = self
            .dynlink_map
            .get(&id)
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        self.get_mut(*id)
    }

    /// Insert a [RunComp].
    pub fn insert(&mut self, mut rc: RunComp) {
        if self.names.contains_key(&rc.name) {
            // TODO
            rc.name = format!("{}-dup", rc.name);
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
    pub fn _get_monitor(&self) -> &RunComp {
        // Unwrap-Ok: this instance is always present.
        self.get(MONITOR_INSTANCE_ID).unwrap()
    }

    /// Get the [RunComp] for the monitor.
    pub fn _get_monitor_mut(&mut self) -> &mut RunComp {
        // Unwrap-Ok: this instance is always present.
        self.get_mut(MONITOR_INSTANCE_ID).unwrap()
    }

    /// Get an iterator over all compartments.
    pub fn _compartments(&self) -> impl Iterator<Item = &RunComp> {
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
        let Ok(rc) = self.get_mut(instance) else {
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
        let Ok(rc) = self.get(instance) else {
            return 0;
        };
        rc.raw_flags()
    }

    fn wait_for_compartment_state_change(
        &self,
        instance: ObjID,
        state: u64,
    ) -> Result<ThreadSyncSleep, TwzError> {
        let rc = self.get(instance)?;
        Ok(rc.until_change(state))
    }

    pub fn main_thread_exited(&mut self, instance: ObjID) {
        tracing::debug!("main thread for compartment {} exited", instance);
        while !self.update_compartment_flags(instance, |old| Some(old | COMP_EXITED)) {}

        let Ok(rc) = self.get(instance) else {
            tracing::warn!("failed to find compartment {} during exit", instance);
            return;
        };
        for dep in rc.deps.clone() {
            self.dec_use_count(dep);
        }

        let Ok(rc) = self.get_mut(instance) else {
            tracing::warn!("failed to find compartment {} during exit", instance);
            return;
        };
        tracing::trace!("runcomp usecount: {}", rc.use_count);
        if rc.use_count == 0 {
            if let Some(rc) = self.remove(instance) {
                self.cleanup_queue.push(rc)
            }
        }
    }

    pub fn dec_use_count(&mut self, instance: ObjID) {
        let Ok(rc) = self.get_mut(instance) else {
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

    pub fn process_cleanup_queue(
        &mut self,
        dynlink: &mut Context,
    ) -> (Vec<Option<Compartment>>, Vec<Vec<LoadedOrUnloaded>>) {
        let (comps, libs) = self
            .cleanup_queue
            .drain(..)
            .map(|c| dynlink.unload_compartment(c.compartment_id))
            .unzip();
        (comps, libs)
    }
}

impl super::Monitor {
    /// Get CompartmentInfo for this caller. Note that this will write to the compartment-thread's
    /// simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_compartment_info(
        &self,
        instance: ObjID,
        thread: ObjID,
        desc: Option<Descriptor>,
    ) -> Result<CompartmentInfo, TwzError> {
        let (_, ref mut comps, ref dynlink, _, ref comphandles) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let comp_id = desc
            .map(|comp| comphandles.lookup(instance, comp).map(|ch| ch.instance))
            .unwrap_or(Some(instance))
            .ok_or(TwzError::INVALID_ARGUMENT)?;

        let name = comps.get(comp_id)?.name.clone();
        let pt = comps.get_mut(instance)?.get_per_thread(thread);
        let name_len = pt.write_bytes(name.as_bytes());
        let comp = comps.get(comp_id)?;
        let nr_libs = dynlink
            .get_compartment(comp.compartment_id)
            .ok()
            .ok_or(TwzError::INVALID_ARGUMENT)?
            .library_ids()
            .count();

        Ok(CompartmentInfo {
            name_len,
            id: comp_id,
            sctx: comp.sctx,
            flags: comp.raw_flags(),
            nr_libs,
        })
    }

    /// Get CompartmentInfo for this caller. Note that this will write to the compartment-thread's
    /// simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_compartment_gate_address(
        &self,
        instance: ObjID,
        thread: ObjID,
        desc: Option<Descriptor>,
        name_len: usize,
    ) -> Result<usize, TwzError> {
        let name = self.read_thread_simple_buffer(instance, thread, name_len)?;
        let (_, ref comps, ref dynlink, _, ref comphandles) =
            *self.locks.lock(ThreadKey::get().unwrap());
        let comp_id = desc
            .map(|comp| comphandles.lookup(instance, comp).map(|ch| ch.instance))
            .unwrap_or(Some(instance))
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        let name = String::from_utf8(name)
            .ok()
            .ok_or(TwzError::INVALID_ARGUMENT)?;

        let comp = comps.get(comp_id)?;
        let dc = dynlink
            .get_compartment(comp.compartment_id)
            .ok()
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        for lid in dc.library_ids() {
            let lib = dynlink
                .get_library(lid)
                .map_err(|_| GenericError::Internal)?;
            if let Some(gates) = lib.iter_secgates() {
                for gate in gates {
                    if gate.name().to_str().ok() == Some(name.as_str()) {
                        return Ok(gate.imp);
                    }
                }
            }
        }
        Err(NamingError::NotFound.into())
    }

    /// Open a compartment handle for this caller compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_compartment_handle(
        &self,
        caller: ObjID,
        compartment: ObjID,
    ) -> Result<Descriptor, TwzError> {
        let (_, ref mut comps, _, _, ref mut ch) = *self.locks.lock(ThreadKey::get().unwrap());
        let comp = comps.get_mut(compartment)?;
        comp.inc_use_count();
        ch.insert(
            caller,
            super::CompartmentHandle {
                instance: if compartment.raw() == 0 {
                    caller
                } else {
                    compartment
                },
            },
        )
        .ok_or(ResourceError::OutOfResources.into())
    }

    /// Open a compartment handle for this caller compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn lookup_compartment(
        &self,
        instance: ObjID,
        thread: ObjID,
        name_len: usize,
    ) -> Result<Descriptor, TwzError> {
        let name = self.read_thread_simple_buffer(instance, thread, name_len)?;
        let name = String::from_utf8(name)
            .ok()
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        let (_, ref mut comps, _, _, ref mut ch) = *self.locks.lock(ThreadKey::get().unwrap());
        let comp = comps.get_name_mut(&name)?;
        comp.inc_use_count();
        ch.insert(
            instance,
            super::CompartmentHandle {
                instance: comp.instance,
            },
        )
        .ok_or(ResourceError::OutOfResources.into())
    }

    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn compartment_wait(&self, caller: ObjID, desc: Option<Descriptor>, flags: u64) -> u64 {
        let Some(instance) = ({
            let comphandles = self._compartment_handles.write(ThreadKey::get().unwrap());
            let comp_id = desc
                .map(|comp| comphandles.lookup(caller, comp).map(|ch| ch.instance))
                .unwrap_or(Some(caller));
            comp_id
        }) else {
            return 0;
        };
        self.wait_for_compartment_state_change(instance, flags);
        self.load_compartment_flags(instance)
    }

    /// Open a handle to the n'th dependency compartment of a given compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_compartment_deps(
        &self,
        caller: ObjID,
        desc: Option<Descriptor>,
        dep_n: usize,
    ) -> Result<Descriptor, TwzError> {
        let dep = {
            let (_, ref mut comps, _, _, ref mut comphandles) =
                *self.locks.lock(ThreadKey::get().unwrap());
            let comp_id = desc
                .map(|comp| comphandles.lookup(caller, comp).map(|ch| ch.instance))
                .unwrap_or(Some(caller))
                .ok_or(ArgumentError::InvalidArgument)?;
            let comp = comps.get_mut(comp_id)?;
            comp.deps.get(dep_n).cloned()
        }
        .ok_or(TwzError::INVALID_ARGUMENT)?;
        self.get_compartment_handle(caller, dep)
    }

    /// Load a new compartment with a root library ID, and return a compartment handle.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn load_compartment(
        &self,
        caller: ObjID,
        thread: ObjID,
        name_len: usize,
        args_len: usize,
        env_len: usize,
        new_comp_flags: NewCompartmentFlags,
    ) -> Result<Descriptor, TwzError> {
        let total_bytes = name_len + args_len + env_len;
        let str_bytes = self.read_thread_simple_buffer(caller, thread, total_bytes)?;
        let name_bytes = &str_bytes[0..name_len];
        let arg_bytes = &str_bytes[name_len..(name_len + args_len)];
        let env_bytes = &str_bytes[(name_len + args_len)..total_bytes];

        let input = String::from_utf8_lossy(&name_bytes);
        let mut split = input.split("::");
        let compname = split.next().ok_or(TwzError::INVALID_ARGUMENT)?;
        let libname = split.next().ok_or(TwzError::INVALID_ARGUMENT)?;
        let root = UnloadedLibrary::new(libname);

        // parse args
        let args_bytes = arg_bytes.split_inclusive(|b| *b == 0);
        let args = args_bytes
            .map(CStr::from_bytes_with_nul)
            .try_collect::<Vec<_>>()
            .map_err(|_| TwzError::INVALID_ARGUMENT)?;
        tracing::debug!("load {}: args: {:?}", compname, args);

        // parse env
        let envs_bytes = env_bytes.split_inclusive(|b| *b == 0);
        let env = envs_bytes
            .map(CStr::from_bytes_with_nul)
            .try_collect::<Vec<_>>()
            .map_err(|_| TwzError::INVALID_ARGUMENT)?;
        tracing::trace!("load {}: env: {:?}", compname, env);

        let extras = env
            .iter()
            .filter_map(|item| {
                let item = item.to_str().ok()?;
                if item.starts_with("LD_PRELOAD=") {
                    Some(UnloadedLibrary::new(item.trim_start_matches("LD_PRELOAD=")))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        tracing::debug!("ld preload extras: {:?}", extras);

        let mondebug = env
            .iter()
            .find(|s| s.to_string_lossy().starts_with("MONDEBUG="))
            .is_some();
        let loader = {
            let mut dynlink = self.dynlink.write(ThreadKey::get().unwrap());
            loader::RunCompLoader::new(*dynlink, compname, root, &extras, new_comp_flags, mondebug)
        }
        .map_err(|_| GenericError::Internal)?;

        let root_comp = {
            let (_, ref mut cmp, ref mut dynlink, _, _) =
                &mut *self.locks.lock(ThreadKey::get().unwrap());
            // TODO: dynlink err map
            loader
                .build_rcs(&mut *cmp, &mut *dynlink, mondebug)
                .map_err(|_| GenericError::Internal)?
        };
        tracing::trace!("loaded {} as {}", compname, root_comp);

        let desc = self.get_compartment_handle(caller, root_comp)?;

        self.start_compartment(root_comp, &args, &env, mondebug)?;

        Ok(desc)
    }

    /// Drop a compartment handle.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn drop_compartment_handle(&self, caller: ObjID, desc: Descriptor) {
        let comps = {
            let (_, ref mut cmgr, ref mut dynlink, _, ref mut comp_handles) =
                *self.locks.lock(ThreadKey::get().unwrap());
            let comp = comp_handles.remove(caller, desc);

            if let Some(comp) = comp {
                cmgr.dec_use_count(comp.instance);
            }
            cmgr.process_cleanup_queue(&mut *dynlink)
        };
        tracing::trace!("HRE");
        drop(comps);
    }

    #[tracing::instrument(skip(self, f), level = tracing::Level::DEBUG)]
    pub fn update_compartment_flags(
        &self,
        instance: ObjID,
        f: impl FnOnce(u64) -> Option<u64>,
    ) -> bool {
        let mut cmp = self.comp_mgr.write(ThreadKey::get().unwrap());
        cmp.update_compartment_flags(instance, f)
    }

    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn load_compartment_flags(&self, instance: ObjID) -> u64 {
        let cmp = self.comp_mgr.write(ThreadKey::get().unwrap());
        cmp.load_compartment_flags(instance)
    }

    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn wait_for_compartment_state_change(&self, instance: ObjID, state: u64) {
        let sl = {
            let cmp = self.comp_mgr.write(ThreadKey::get().unwrap());
            let Ok(sl) = cmp.wait_for_compartment_state_change(instance, state) else {
                return;
            };

            if sl.ready() {
                return;
            }
            drop(cmp);
            sl
        };

        let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(sl)], None);
    }
}

/// A handle to a compartment.
pub struct CompartmentHandle {
    pub instance: ObjID,
}
