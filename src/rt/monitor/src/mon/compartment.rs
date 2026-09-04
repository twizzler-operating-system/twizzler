use std::{collections::HashMap, ffi::CStr, sync::atomic::Ordering, time::Instant};

use dynlink::{
    compartment::{Compartment, CompartmentId},
    context::{Context, LoadedOrUnloaded, NewCompartmentFlags},
    library::UnloadedLibrary,
};
use happylock::ThreadKey;
use monitor_api::{
    CompartmentInfoRaw, CompartmentLoaderConfig, CompartmentMgrStats, ControllerOption, ThreadInfo,
    MONITOR_INSTANCE_ID,
};
use secgate::util::Descriptor;
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_preload_range, sys_thread_change_state, sys_thread_sync, PreloadRangeSpec,
        ThreadSync, MAX_PRELOAD_RANGES,
    },
};
use twizzler_rt_abi::{
    error::{ArgumentError, GenericError, NamingError, ResourceError, TwzError},
    object::{MapFlags, ObjID},
};

use super::thread::ThreadMgr;

mod compconfig;
mod compthread;
mod loader;
mod runcomp;

pub use compconfig::*;
pub(crate) use compthread::StackObject;
pub use runcomp::*;

/// Switch for prefaulting the root object before a compartment load (`COMPNEW.md` plan A).
///
/// **Off because it was measured and does not pay.** release-kvm-smp4, 3 rounds each, ~200
/// compartment loads per arm, this switch the only difference: whole-load median 16.31 ms on vs
/// 16.47 ms off. The root-load phase does move the predicted way (7.04 vs 7.75 ms median), but
/// relocation moves back by about the same (7.63 vs 6.93), so nothing reaches the whole load. The
/// tail got worse, not better: worst root-load 121.8 ms with it on against 18.6 ms with it off,
/// which is the "prefetch of a superset" risk the plan flagged.
///
/// Left in place, switched off, alongside the counters that measured it: the premise (relocation
/// waits on source-object COW faults) is only half-refuted, and the next attempt needs this.
/// Switch for the per-spawn phase counter (`SPAWNPHA`): parse / load / start.
///
/// `start_compartment` **blocks until the new compartment signals `COMP_READY`**, so
/// `monitor_rt_load_compartment` -- and therefore `Command::spawn` -- does not return until the
/// child's runtime has come up. That makes "the load" and "the child booting" two different costs
/// inside one number, and `LOAD_PHASE_STATS` can only see the first of them. Off by default; the
/// values are microseconds.
pub(crate) const SPAWN_PHASE_STATS: bool = false;

/// Monitor half of the spawn-latency join (`SPAWNGO`); the child half is `CHILDTOP`, behind
/// `twz_rt`'s `SPAWN_LAT_STATS`. **Flip both together** -- two crates, one measurement. One
/// record per side per spawn, deliberately not folded into [`SPAWN_PHASE_STATS`] so the window
/// can be read without arming the higher-volume phase counters around it.
pub(crate) const SPAWN_LAT_STATS: bool = false;

const PREFAULT_ROOT: bool = false;

/// Prefault the PT_LOAD ranges of a compartment's root object, outside the dynlink lock.
///
/// Best-effort throughout: every failure here just leaves the pages to be demand-faulted later,
/// which is what happened before this existed.
///
/// **The returned handle must be held until the load finishes.** `Space::map` refcounts by
/// `MapInfo`, and this asks for exactly the mapping dynlink's `load_object` asks for moments later.
/// Dropping it here first takes the count to zero, which removes the cache entry and hands the slot
/// to the background unmapper -- which then races dynlink's fresh map of the same object onto a
/// recycled slot and unmaps it underneath. Holding it keeps the count above zero, so dynlink's map
/// is a cache hit rather than a second syscall.
#[must_use = "dropping the handle early unmaps the object out from under the load"]
fn prefault_root_object(root_object: ObjID) -> Option<super::space::MapHandle> {
    if !PREFAULT_ROOT {
        return None;
    }
    let handle = super::space::Space::map(
        &super::get_monitor().space,
        super::space::MapInfo {
            id: root_object,
            flags: MapFlags::READ,
        },
        ObjID::new(0),
    )
    .ok()?;

    // The ELF image starts at the data base, not the slot base: `monitor_data_start` is the
    // object's null page, which is unmapped by design. This mirrors `dynlink::engines::Backing`,
    // which stores the slot base and adds NULLPAGE_SIZE in `data()`.
    let data = unsafe {
        core::slice::from_raw_parts(
            handle.monitor_data_base() as *const u8,
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    };

    // Every path from here returns the handle, including the failure paths: once the map exists,
    // dropping it before dynlink takes its own reference is the race described above.
    match dynlink::library::pt_load_ranges(data) {
        Err(e) => {
            tracing::debug!(
                "prefault {}: could not read program headers: {}",
                root_object,
                e
            );
        }
        Ok(ranges) => {
            // File offset N lives at object offset NULLPAGE_SIZE + N; see `engines::twizzler`.
            let specs: Vec<_> = ranges
                .iter()
                .take(MAX_PRELOAD_RANGES)
                .map(|(off, len)| PreloadRangeSpec::from_bytes(NULLPAGE_SIZE as u64 + off, *len))
                .collect();
            if !specs.is_empty() {
                let _ = sys_object_preload_range(root_object, &specs)
                    .inspect_err(|e| tracing::debug!("prefault {} failed: {}", root_object, e));
            }
        }
    }

    Some(handle)
}

/// Manages compartments.
#[derive(Default)]
pub struct CompartmentMgr {
    names: HashMap<String, ObjID>,
    instances: HashMap<ObjID, RunComp>,
    controllers: HashMap<ObjID, Vec<ObjID>>,
    dynlink_map: HashMap<CompartmentId, ObjID>,
    cleanup_queue: Vec<RunComp>,
    /// Cleanup passes a queued compartment has waited through, so one that never becomes reapable
    /// is reported rather than just absent. See [CompartmentMgr::process_cleanup_queue].
    cleanup_waits: HashMap<ObjID, u32>,
}

impl CompartmentMgr {
    /// Get a [RunComp] by instance ID.
    pub fn get(&self, id: ObjID) -> Result<&RunComp, TwzError> {
        self.instances.get(&id).ok_or(TwzError::INVALID_ARGUMENT)
    }

    /// The instance ID of every compartment currently known, in no particular order.
    pub fn instance_ids(&self) -> Vec<ObjID> {
        self.instances.keys().copied().collect()
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
        self.remove_from_controllers(rc.instance);
        if let Some(controller) = rc.controller {
            tracing::debug!(
                "setting controller for new compartment {}: {}",
                rc.instance,
                controller
            );
            self.controllers
                .entry(controller)
                .or_default()
                .push(rc.instance);
        }
        self.instances.insert(rc.instance, rc);
    }

    fn remove_from_controllers(&mut self, id: ObjID) {
        for c in self.controllers.iter_mut() {
            c.1.retain(|t| *t != id);
        }
    }

    /// Remove a [RunComp].
    pub fn remove(&mut self, id: ObjID) -> Option<RunComp> {
        let rc = self.instances.remove(&id)?;
        self.names.remove(&rc.name);
        self.dynlink_map.remove(&rc.compartment_id);
        self.remove_from_controllers(id);
        Some(rc)
    }

    pub fn set_controller(&mut self, target: ObjID, controller: ObjID) -> Result<(), TwzError> {
        let comp = self.get_mut(target)?;
        comp.controller = Some(controller);
        comp.set_config_controller(ControllerOption::Object(controller));
        tracing::debug!(
            "setting controller for compartment {}: {}",
            target,
            controller
        );
        self.remove_from_controllers(target);
        self.controllers.entry(controller).or_default().push(target);
        Ok(())
    }

    pub fn find_controller_targets(&self, controller: ObjID) -> Vec<ObjID> {
        self.controllers
            .get(&controller)
            .cloned()
            .unwrap_or_default()
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
    pub fn compartments(&self) -> impl Iterator<Item = &RunComp> {
        self.instances.values()
    }

    pub fn compartments_mut(&mut self) -> impl Iterator<Item = &mut RunComp> {
        self.instances.values_mut()
    }

    /// Takes `&self`: a compartment's flags are an `AtomicU64`, so updating them needs no exclusive
    /// access to the manager. That lets every caller hold a *read* of it -- and the wake syscall
    /// inside `cas_flag` then no longer runs under a lock that excludes all other compartments.
    fn update_compartment_flags(
        &self,
        instance: ObjID,
        f: impl FnOnce(u64) -> Option<u64>,
    ) -> bool {
        let Ok(rc) = self.get(instance) else {
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
    ) -> Result<[ThreadSync; 2], TwzError> {
        let rc = self.get(instance)?;
        Ok(rc.until_change(state))
    }

    pub fn main_thread_exited(&mut self, instance: ObjID, also: &[ObjID]) {
        tracing::debug!("main thread for compartment {} exited", instance);
        // `update_compartment_flags` reports false both for "the CAS lost" and for "no such
        // compartment"; retrying unconditionally turns the latter into an infinite spin under every
        // monitor lock, which the `get` below already knows how to handle.
        while self.get(instance).is_ok()
            && !self.update_compartment_flags(instance, |old| Some(old | COMP_EXITED))
        {}

        let Ok(rc) = self.get(instance) else {
            tracing::warn!("failed to find compartment {} during exit", instance);
            return;
        };

        // `per_thread` only holds threads that have called a gate needing the simple buffer, so it
        // is a *subset* of the compartment's threads -- and killing a subset is worse than killing
        // none. The socket engine's poll thread calls `net_release_port`, so it is in the set and
        // dies; a thread that only maps objects and does socket I/O is not, and lives on blocked
        // forever on an engine condvar nothing will ever notify again. `also` closes that gap with
        // the thread manager's own record of who was spawned for this instance.
        // Collected before the loop: each iteration makes a syscall, and holding the compartment's
        // per-thread lock across those would block every gate call in it behind the teardown.
        for thread in rc.thread_ids_including(also) {
            crate::lockdiag::note_killed(thread);
            // A gate call runs this thread inside another compartment (this one included, which
            // is why the wedge takes the whole system with it), and an exit landing there leaves
            // that compartment's locks held by a corpse. The kernel holds the request until the
            // thread is running its own code again -- delivery is gated on the home context
            // stamped at spawn (`ThreadSpawnArgs::home_sctx`).
            let _ = sys_thread_change_state(thread, twizzler_abi::thread::ExecutionState::Exited);
        }

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
                self.cleanup_queue.push(rc);
                CLEANUP_BACKLOG.store(self.cleanup_queue.len(), Ordering::Relaxed);
            }
        }
    }

    /// Returns whether this drop queued a compartment for teardown, i.e. whether a cleanup pass
    /// has anything new to do.
    pub fn dec_use_count(&mut self, instance: ObjID) -> bool {
        let Ok(rc) = self.get_mut(instance) else {
            return false;
        };

        let z = rc.dec_use_count();
        let ex = rc.has_flag(COMP_EXITED);
        if z && ex {
            if let Some(rc) = self.remove(instance) {
                self.cleanup_queue.push(rc);
                CLEANUP_BACKLOG.store(self.cleanup_queue.len(), Ordering::Relaxed);
                return true;
            }
        }
        false
    }

    pub fn stat(&self) -> CompartmentMgrStats {
        CompartmentMgrStats {
            nr_compartments: self.instances.len(),
        }
    }

    /// Unload every queued compartment that has no threads left, and keep the rest queued.
    ///
    /// `main_thread_exited` requests its threads' exits through `sys_thread_change_state`,
    /// which the kernel holds until the target is running its own code again -- so a queued
    /// compartment can still have threads standing on it. Unloading it there dropped its
    /// `StackObject`'s handle, and the unmapper took a stack out from under a thread that was still
    /// executing on it: a memory fault in monitor code, reported as an `unwrap` panic in
    /// `upcall_handle` while the compartment lock was held, which wedged the system.
    ///
    /// `use_count` cannot stand in for this: it counts handles and deps, not live threads.
    ///
    /// The cleaner thread calls this after reaping each thread, so the last exit of a compartment
    /// is what drives the pass that finally unloads it.
    pub fn process_cleanup_queue(
        &mut self,
        tmgr: &ThreadMgr,
        dynlink: &mut Context,
    ) -> (Vec<Option<Compartment>>, Vec<Vec<LoadedOrUnloaded>>) {
        let (ready, pending): (Vec<_>, Vec<_>) = self
            .cleanup_queue
            .drain(..)
            .partition(|rc| tmgr.threads_of(rc.instance).is_empty());

        for rc in &pending {
            // A thread that never takes its force-exit -- parked on a mutex, the pager, or the
            // memory tracker, none of which poll for it -- keeps its compartment here forever.
            // Leaking it beats unmapping under it, but silently leaking it beats nothing.
            let waits = self.cleanup_waits.entry(rc.instance).or_default();
            *waits += 1;
            if *waits % 1000 == 0 {
                tracing::warn!(
                    "compartment {} ({}) still has {} live thread(s) after {} cleanup passes",
                    rc.instance,
                    rc.name,
                    tmgr.threads_of(rc.instance).len(),
                    waits,
                );
            }
        }
        for rc in &ready {
            self.cleanup_waits.remove(&rc.instance);
        }

        self.cleanup_queue = pending;
        CLEANUP_BACKLOG.store(self.cleanup_queue.len(), Ordering::Relaxed);
        let (comps, libs) = ready
            .into_iter()
            .map(|c| {
                CLEANUPS_DONE.fetch_add(1, Ordering::Relaxed);
                dynlink.unload_compartment(c.compartment_id)
            })
            .unzip();
        (comps, libs)
    }
}

/// Mirror of `cleanup_queue.len()`, readable without the monitor lock collection. Drives the
/// cleaner thread's self-boost ([super::thread::cleaner]): the reclaim3 census showed ~15k dead
/// compartments' contexts still holding their mappings (92% of RAM pending-delete) with zero
/// stuck-thread warnings — teardown was purely outrun by production at User priority.
pub(crate) static CLEANUP_BACKLOG: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Compartments fully processed through `process_cleanup_queue` (unload reached).
pub(crate) static CLEANUPS_DONE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// `RunComp::drop` executions — the instance-delete trigger.
pub(crate) static RUNCOMP_DROPS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

impl super::Monitor {
    /// Get CompartmentInfo for this caller. Note that this will write to the compartment-thread's
    /// simple buffer.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_compartment_info(
        &self,
        instance: ObjID,
        thread: ObjID,
        desc: Option<Descriptor>,
    ) -> Result<CompartmentInfoRaw, TwzError> {
        tracing::trace!(
            "get_compartment_info: instance = {}, thread = {}, desc = {:?}",
            instance,
            thread,
            desc
        );
        // `dynlink` is needed for exactly one field, `nr_libs`, which only changes when a library
        // is loaded into the compartment -- so it is cached on the `RunComp` and this reads the two
        // locks it actually needs. Writing the name into the caller's own simple buffer does not
        // need `&mut RunComp`, and nothing else here mutates.
        let build = |comps: &CompartmentMgr,
                     comphandles: &secgate::util::HandleMgr<CompartmentHandle>,
                     nr_libs: Option<usize>|
         -> Result<Result<CompartmentInfoRaw, CompartmentId>, TwzError> {
            let comp_id = desc
                .map(|comp| comphandles.lookup(instance, comp).map(|ch| ch.instance))
                .unwrap_or(Some(instance))
                .ok_or(TwzError::INVALID_ARGUMENT)?;

            let comp = comps.get(comp_id)?;
            let Some(nr_libs) = nr_libs.or_else(|| comp.cached_nr_libs()) else {
                // Caller must go compute it under `dynlink`.
                return Ok(Err(comp.compartment_id));
            };

            let name = comp.name.clone();
            if comps.get(instance).is_err() {
                tracing::warn!(
                    "get_compartment_info: instance {} not found in comps",
                    instance
                );
            }
            super::ptstats::record(super::ptstats::Site::CompInfo);
            let pt = comps.get(instance)?.get_per_thread(thread);
            let name_len = pt.lock().unwrap().write_bytes(name.as_bytes());
            Ok(Ok(CompartmentInfoRaw {
                name_len,
                id: comp_id,
                sctx: comp.sctx,
                flags: comp.raw_flags(),
                nr_libs,
                exit_code: comp.read_error_code(),
            }))
        };

        {
            let (ref comps, ref comphandles) =
                *crate::lockdiag::watched(self.comp_lookup.read(super::reentrant_key()?));
            if let Ok(info) = build(comps, comphandles, None)? {
                return Ok(info);
            }
        }

        // Cold: count the libraries once, cache it, and answer from the same read.
        let (_, ref comps, ref dynlink, _, ref comphandles) =
            *crate::lockdiag::watched(self.locks.read(super::reentrant_key()?));
        let comp_id = desc
            .map(|comp| comphandles.lookup(instance, comp).map(|ch| ch.instance))
            .unwrap_or(Some(instance))
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        let nr_libs = dynlink
            .get_compartment(comps.get(comp_id)?.compartment_id)
            .ok()
            .ok_or(TwzError::INVALID_ARGUMENT)?
            .library_ids()
            .count();
        comps.get(comp_id)?.cache_nr_libs(nr_libs);
        build(comps, comphandles, Some(nr_libs))?.map_err(|_| GenericError::Internal.into())
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
        let name = self.read_thread_simple_buffer(
            instance,
            thread,
            name_len,
            super::ptstats::Site::GateAddr,
        )?;
        let name = String::from_utf8(name)
            .ok()
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        self.gate_address_named(instance, desc, &name)
    }

    /// Resolve a secgate address by name, without the simple buffer.
    ///
    /// The buffer round trip was the whole cost of this call: `dynamic_gate` is resolved once per
    /// gate per compartment (~14 times each, measured), and a gate symbol name is tens of bytes.
    /// `InlineName` carries it in the gate arguments instead.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn gate_address_named(
        &self,
        instance: ObjID,
        desc: Option<Descriptor>,
        name: &str,
    ) -> Result<usize, TwzError> {
        // Cache first, under the two locks this actually needs. A gate's address does not move
        // unless a library is loaded into the compartment, and `RunComp::invalidate_dynlink_cache`
        // covers that -- so the steady state never reads `dynlink`, and never rescans every
        // library's gates. See `RunComp::gate_cache`.
        {
            let (ref comps, ref comphandles) =
                *crate::lockdiag::watched(self.comp_lookup.read(super::reentrant_key()?));
            let comp_id = desc
                .map(|comp| comphandles.lookup(instance, comp).map(|ch| ch.instance))
                .unwrap_or(Some(instance))
                .ok_or(TwzError::INVALID_ARGUMENT)?;
            if let Some(addr) = comps.get(comp_id)?.cached_gate(name) {
                return Ok(addr);
            }
        }

        let (_, ref comps, ref dynlink, _, ref comphandles) =
            *crate::lockdiag::watched(self.locks.read(super::reentrant_key()?));
        let comp_id = desc
            .map(|comp| comphandles.lookup(instance, comp).map(|ch| ch.instance))
            .unwrap_or(Some(instance))
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
                    if gate.name().to_str() == Ok(name) {
                        // Only successful lookups are cached: a miss can become a hit when a
                        // library is loaded, and caching the absence would need invalidation on a
                        // path that has none.
                        comp.cache_gate(name, gate.imp);
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
        let (ref mut comps, ref mut ch) =
            *crate::lockdiag::watched(self.comp_lookup.lock(super::reentrant_key()?));
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
    pub fn lookup_compartment_id(
        &self,
        instance: ObjID,
        thread: ObjID,
        comp: ObjID,
    ) -> Result<Descriptor, TwzError> {
        let (ref mut comps, ref mut ch) =
            *crate::lockdiag::watched(self.comp_lookup.lock(ThreadKey::get().unwrap()));
        let comp = comps.get_mut(comp)?;
        comp.inc_use_count();
        ch.insert(
            instance,
            super::CompartmentHandle {
                instance: comp.instance,
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
        let name = self.read_thread_simple_buffer(
            instance,
            thread,
            name_len,
            super::ptstats::Site::LookupComp,
        )?;
        let name = String::from_utf8(name)
            .ok()
            .ok_or(TwzError::INVALID_ARGUMENT)?;
        self.lookup_compartment_named(instance, &name)
    }

    /// Open a handle to a compartment by name, without the simple buffer. See
    /// [`Self::gate_address_named`] for why.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn lookup_compartment_named(
        &self,
        instance: ObjID,
        name: &str,
    ) -> Result<Descriptor, TwzError> {
        let (ref mut comps, ref mut ch) =
            *crate::lockdiag::watched(self.comp_lookup.lock(ThreadKey::get().unwrap()));
        let comp = comps.get_name_mut(name)?;
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
    pub fn compartment_wait(
        &self,
        caller: ObjID,
        desc: Option<Descriptor>,
        flags: u64,
    ) -> (u64, u64) {
        let Some(instance) = ({
            let comphandles = crate::lockdiag::watched(
                self._compartment_handles.write(ThreadKey::get().unwrap()),
            );
            let comp_id = desc
                .map(|comp| comphandles.lookup(caller, comp).map(|ch| ch.instance))
                .unwrap_or(Some(caller));
            comp_id
        }) else {
            return (0, 0);
        };
        self.wait_for_compartment_state_change(instance, flags);
        self.read_flags_and_signals(instance)
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
            // Reads `comps` and `comphandles` and nothing else, so it takes those two rather than
            // a read of the whole collection -- which includes `dynlink`, held for a write across a
            // median 31 ms compartment load (`sysperf.md` round 8).
            let (ref comps, ref comphandles) =
                *crate::lockdiag::watched(self.comp_lookup.read(super::reentrant_key()?));
            let comp_id = desc
                .map(|comp| comphandles.lookup(caller, comp).map(|ch| ch.instance))
                .unwrap_or(Some(caller))
                .ok_or(ArgumentError::InvalidArgument)?;
            let comp = comps.get(comp_id)?;
            comp.deps.get(dep_n).cloned()
        }
        .ok_or(TwzError::INVALID_ARGUMENT)?;
        self.get_compartment_handle(caller, dep)
    }

    /// Get the n'th thread's info from a compartment.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn get_compartment_thread_info(
        &self,
        caller: ObjID,
        desc: Option<Descriptor>,
        t_n: usize,
    ) -> Result<ThreadInfo, TwzError> {
        let dep = {
            // Reads `comps` and `comphandles` and nothing else, so it takes those two rather than
            // a read of the whole collection -- which includes `dynlink`, held for a write across a
            // median 31 ms compartment load (`sysperf.md` round 8).
            let (ref comps, ref comphandles) =
                *crate::lockdiag::watched(self.comp_lookup.read(super::reentrant_key()?));
            let comp_id = desc
                .map(|comp| comphandles.lookup(caller, comp).map(|ch| ch.instance))
                .unwrap_or(Some(caller))
                .ok_or(ArgumentError::InvalidArgument)?;
            let comp = comps.get(comp_id)?;
            comp.get_nth_thread_info(t_n)
        }
        .ok_or(TwzError::INVALID_ARGUMENT);
        dep
    }

    /// Load a new compartment with a root library ID, and return a compartment handle.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn load_compartment(
        &self,
        caller: ObjID,
        thread: ObjID,
        root_object: ObjID,
        name_len: usize,
        args_len: usize,
        env_len: usize,
        new_comp_flags: NewCompartmentFlags,
        config: *const CompartmentLoaderConfig,
    ) -> Result<Descriptor, TwzError> {
        // TODO: verify config pointer
        let _start_1 = Instant::now();
        let config = unsafe { config.read() };
        let total_bytes = name_len + args_len + env_len;
        let str_bytes = self.read_thread_simple_buffer(
            caller,
            thread,
            total_bytes,
            super::ptstats::Site::LoadComp,
        )?;
        let name_bytes = &str_bytes[0..name_len];
        let arg_bytes = &str_bytes[name_len..(name_len + args_len)];
        let env_bytes = &str_bytes[(name_len + args_len)..total_bytes];

        let input = String::from_utf8_lossy(&name_bytes);
        let mut split = input.split("::");
        let compname = split.next().ok_or(TwzError::INVALID_ARGUMENT)?;
        let libname = split.next().ok_or(TwzError::INVALID_ARGUMENT)?;
        let root = UnloadedLibrary::new_object(libname, root_object);

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
        let extras_sctx = env
            .iter()
            .filter_map(|item| {
                let item = item.to_str().ok()?;
                if item.starts_with("SCTX_PRELOAD=") {
                    Some(UnloadedLibrary::new(
                        item.trim_start_matches("SCTX_PRELOAD="),
                    ))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        tracing::debug!("sctx preload extras: {:?}", extras);

        let mondebug = env
            .iter()
            .find(|s| s.to_string_lossy().starts_with("MONDEBUG="))
            .is_some();

        // Make the root binary's loadable pages resident before taking the lock. `load_segments`
        // installs the new text/data objects as COW copy-specs of this object, so the first write
        // during relocation faults through to here -- and that used to be a disk read taken under
        // the dynlink write lock. The root binary is the only library this matters for: the shared
        // ones are resident after the first compartment (`sysperf.md` round 8).
        // Held until the end of this function: see `prefault_root_object`.
        let _prefault = prefault_root_object(root_object);

        let _start_2 = Instant::now();
        // Spawn-side lock *wait* probe (`LCKWAIT`, vals = [site]): MONHOLD names holders, but the
        // tail question is whether a spawn queues behind them at all. Sites: 1 = dynlink.write,
        // 2 = dynlink.read, 3 = locks.lock (build_rcs), 4/5 = start_compartment's two.
        let lck_wait = |site: u64, t0: Option<Instant>| {
            if let Some(t0) = t0 {
                secgate::statlog::record_on(
                    SPAWN_PHASE_STATS,
                    "LCKWAIT",
                    t0.elapsed().as_micros() as u64,
                    &[site],
                );
            }
        };
        // Two phases, two locks. Graph mutation needs the write; relocation does not, and is a
        // median 6.7 ms of the load that every reader of the lock collection used to queue behind.
        let pending = {
            let _t = SPAWN_PHASE_STATS.then(Instant::now);
            let mut dynlink =
                crate::lockdiag::watched(self.dynlink.write(ThreadKey::get().unwrap()));
            lck_wait(1, _t);
            loader::RunCompLoader::load_graph(
                *dynlink,
                compname,
                root,
                &extras,
                &extras_sctx,
                new_comp_flags,
                mondebug,
            )
        }
        .inspect_err(|e| tracing::error!("failed to load new compartment: {}", e))
        .map_err(|_| GenericError::Internal)?;

        let loader = {
            let _t = SPAWN_PHASE_STATS.then(Instant::now);
            let dynlink = crate::lockdiag::watched(self.dynlink.read(ThreadKey::get().unwrap()));
            lck_wait(2, _t);
            pending.relocate_and_finish(*dynlink, compname, mondebug)
        }
        .inspect_err(|e| tracing::error!("failed to relocate new compartment: {}", e))
        .map_err(|_| GenericError::Internal)?;

        let root_comp = {
            let _t = SPAWN_PHASE_STATS.then(Instant::now);
            let (_, ref mut cmp, ref mut dynlink, _, _) =
                &mut *crate::lockdiag::watched(self.locks.lock(ThreadKey::get().unwrap()));
            lck_wait(3, _t);

            let controller = match config.controller {
                ControllerOption::Inherit => cmp.get(caller)?.controller,
                ControllerOption::NoController => None,
                ControllerOption::Object(id) => Some(id),
            };
            // TODO: dynlink err map
            loader
                .build_rcs(
                    &mut *cmp,
                    &mut *dynlink,
                    mondebug,
                    new_comp_flags.contains(NewCompartmentFlags::DEBUG),
                    controller,
                    config,
                )
                .inspect_err(|e| tracing::error!("failed to setup new compartment: {}", e))
                .map_err(|_| GenericError::Internal)?
        };
        tracing::trace!("loaded {} as {}", compname, root_comp);

        let desc = self.get_compartment_handle(caller, root_comp)?;

        let _start_3 = Instant::now();
        self.start_compartment(
            root_comp,
            &args,
            &env,
            mondebug,
            new_comp_flags.contains(NewCompartmentFlags::DEBUG),
        )
        .inspect_err(|e| tracing::error!("failed to start new compartment: {}", e))?;
        tracing::trace!(
            "parse strings in {}ms, load in {}ms, start in {}ms",
            (_start_2 - _start_1).as_millis(),
            (_start_3 - _start_2).as_millis(),
            _start_3.elapsed().as_millis()
        );
        secgate::statlog::record_on(
            SPAWN_PHASE_STATS,
            "SPAWNPHA",
            _start_1.elapsed().as_micros() as u64,
            &[
                (_start_2 - _start_1).as_micros() as u64,
                (_start_3 - _start_2).as_micros() as u64,
                _start_3.elapsed().as_micros() as u64,
            ],
        );

        Ok(desc)
    }

    /// Drop a compartment handle.
    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn drop_compartment_handle(&self, caller: ObjID, desc: Descriptor) {
        // See Monitor::drop_library_handle: reached from a panicking thread's backtrace walk,
        // which still holds the key. Leaking the descriptor beats aborting the panic.
        let Ok(key) = super::reentrant_key() else {
            tracing::warn!(
                "skipping drop of compartment handle {} for {}: monitor locks already held by this thread",
                desc,
                caller
            );
            return;
        };
        // Two phases, because the common drop has nothing to clean up. Dropping a handle only
        // queues a teardown when it takes the last use of an already-exited compartment; every
        // other drop leaves the queue exactly as it was. Running the pass anyway took all four
        // locks -- including `dynlink`, which a compartment load holds for a median 31 ms -- and
        // re-partitioned every *pending* entry through `threads_of`, an O(live threads) scan that
        // allocates. A handle drop cannot change thread liveness, so that scan could never find
        // anything a previous pass had not: only a thread exit turns pending into ready, and the
        // cleaner wakes on exactly that. Measured at 838 holds of 1-5 ms per pair of runs
        // (`sysperf.md` round 8).
        let queued = {
            let (ref mut cmgr, ref mut comp_handles) =
                *crate::lockdiag::watched(self.comp_lookup.lock(key));
            match comp_handles.remove(caller, desc) {
                Some(comp) => {
                    tracing::trace!(
                        "dropping compartment handle for {}: {:?}",
                        caller,
                        cmgr.get(comp.instance).map(|c| c.name.clone()),
                    );
                    cmgr.dec_use_count(comp.instance)
                }
                None => false,
            }
        };
        if !queued {
            return;
        }
        // Dropped outside the lock, as before: releasing the unloaded compartments' objects and
        // handles reaches the unmapper and is not something to do while holding the collection.
        let comps = {
            let Ok(key) = super::reentrant_key() else {
                return;
            };
            let (ref tmgr, ref mut cmgr, ref mut dynlink, _, _) =
                *crate::lockdiag::watched(self.locks.lock(key));
            cmgr.process_cleanup_queue(tmgr, &mut *dynlink)
        };
        drop(comps);
    }

    #[tracing::instrument(skip(self, f), level = tracing::Level::DEBUG)]
    pub fn update_compartment_flags(
        &self,
        instance: ObjID,
        f: impl FnOnce(u64) -> Option<u64>,
    ) -> bool {
        let cmp = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
        cmp.update_compartment_flags(instance, f)
    }

    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn load_compartment_flags(&self, instance: ObjID) -> u64 {
        let cmp = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
        cmp.load_compartment_flags(instance)
    }

    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn read_flags_and_signals(&self, instance: ObjID) -> (u64, u64) {
        let cmp = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
        let flags = cmp.load_compartment_flags(instance);
        let signals = cmp
            .get(instance)
            .map(|c| unsafe { &*c.comp_config_ptr() }.read_posted_signals())
            .unwrap_or(0);
        (flags, signals)
    }

    #[tracing::instrument(skip(self), level = tracing::Level::DEBUG)]
    pub fn wait_for_compartment_state_change(&self, instance: ObjID, state: u64) {
        let mut sl = {
            let cmp = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
            let Ok(sl) = cmp.wait_for_compartment_state_change(instance, state) else {
                return;
            };

            if sl.iter().any(|sl| sl.ready()) {
                return;
            }
            drop(cmp);
            sl
        };

        let _ = sys_thread_sync(&mut sl, None);
    }
}

/// A handle to a compartment.
pub struct CompartmentHandle {
    pub instance: ObjID,
}
