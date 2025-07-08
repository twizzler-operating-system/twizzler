use std::{collections::HashSet, ffi::CStr, ptr::null_mut};

use dynlink::{
    compartment::CompartmentId,
    context::{Context, LoadIds, NewCompartmentFlags},
    engines::LoadCtx,
    library::{AllowedGates, LibraryId, UnloadedLibrary},
    DynlinkError,
};
use happylock::ThreadKey;
use monitor_api::SharedCompConfig;
use twizzler_rt_abi::{
    core::{CtorSet, RuntimeInfo},
    error::{GenericError, TwzError},
    object::{MapFlags, ObjID},
};

use super::{
    CompConfigObject, CompartmentMgr, RunComp, StackObject, COMP_DESTRUCTED, COMP_EXITED,
    COMP_IS_BINARY, COMP_READY,
};
use crate::mon::{
    get_monitor,
    space::{MapHandle, Space},
    thread::DEFAULT_STACK_SIZE,
    Monitor,
};

/// Tracks info for loaded, but not yet running, compartments.
#[derive(Debug)]
pub struct RunCompLoader {
    loaded_extras: Vec<LoadInfo>,
    root_comp: LoadInfo,
}

/// A single compartment, loaded but not yet running.
#[derive(Debug, Clone)]
struct LoadInfo {
    // root (eg executable) library ID
    #[allow(dead_code)]
    root_id: LibraryId,
    // runtime library ID (maybe injected)
    #[allow(dead_code)]
    rt_id: LibraryId,
    // security context ID
    sctx_id: ObjID,
    name: String,
    comp_id: CompartmentId,
    // all constructors for all libraries
    ctor_info: Vec<CtorSet>,
    // entry point to call for the runtime to init this compartment
    entry: extern "C" fn(*const RuntimeInfo) -> !,
    is_binary: bool,
}

impl LoadInfo {
    fn new(
        dynlink: &mut Context,
        root_id: LibraryId,
        rt_id: LibraryId,
        sctx_id: ObjID,
        is_binary: bool,
        extras: &[LibraryId],
    ) -> Result<Self, DynlinkError> {
        let lib = dynlink.get_library(rt_id)?;
        let extra_ctors: Vec<_> = extras
            .iter()
            .map(|extra| dynlink.build_ctors_list(*extra, Some(lib.compartment())))
            .try_collect()?;
        let mut root_ctors = dynlink.build_ctors_list(root_id, Some(lib.compartment()))?;
        let mut ctor_info: Vec<_> = extra_ctors.iter().flatten().copied().collect();
        ctor_info.append(&mut root_ctors);
        Ok(Self {
            root_id,
            rt_id,
            comp_id: lib.compartment(),
            sctx_id,
            name: dynlink.get_compartment(lib.compartment())?.name.clone(),
            ctor_info,
            entry: lib.get_entry_address()?,
            is_binary,
        })
    }

    fn build_runcomp(
        &self,
        handle: MapHandle,
        stack_object: StackObject,
    ) -> Result<RunComp, DynlinkError> {
        let comp_config =
            CompConfigObject::new(handle, SharedCompConfig::new(self.sctx_id, null_mut()));

        let flags = if self.is_binary { COMP_IS_BINARY } else { 0 };
        Ok(RunComp::new(
            self.sctx_id,
            self.sctx_id,
            self.name.clone(),
            self.comp_id,
            vec![],
            comp_config,
            flags,
            stack_object,
            self.entry as usize,
            &self.ctor_info,
        ))
    }
}

impl Drop for RunCompLoader {
    fn drop(&mut self) {
        tracing::warn!("drop RunCompLoader: TODO");
    }
}

const RUNTIME_NAME: &str = "libtwz_rt.so";

impl RunCompLoader {
    // the runtime library might be in the dependency tree from the shared object files.
    // if not, we need to insert it.
    fn maybe_inject_runtime(
        dynlink: &mut Context,
        root_id: LibraryId,
        comp_id: CompartmentId,
        load_ctx: &mut LoadCtx,
    ) -> Result<LibraryId, DynlinkError> {
        if let Some(id) = dynlink.lookup_library(comp_id, RUNTIME_NAME) {
            return Ok(id);
        }

        let rt_unlib = UnloadedLibrary::new(RUNTIME_NAME);
        let loads = dynlink.load_library_in_compartment(
            comp_id,
            rt_unlib,
            AllowedGates::Private,
            load_ctx,
        )?;
        dynlink.add_manual_dependency(root_id, loads[0].lib);
        Ok(loads[0].lib)
    }

    /// Build a new RunCompLoader. This will load and relocate libraries in the dynamic linker, but
    /// won't start compartment threads.
    pub fn new(
        dynlink: &mut Context,
        comp_name: &str,
        root_unlib: UnloadedLibrary,
        extras: &[UnloadedLibrary],
        new_comp_flags: NewCompartmentFlags,
        mondebug: bool,
    ) -> miette::Result<Self> {
        struct UnloadOnDrop(Vec<LoadIds>);
        impl Drop for UnloadOnDrop {
            fn drop(&mut self) {
                tracing::warn!("todo: drop");
            }
        }
        let root_comp_id = dynlink.add_compartment(comp_name, new_comp_flags)?;
        let allowed_gates = if new_comp_flags.contains(NewCompartmentFlags::EXPORT_GATES) {
            AllowedGates::Public
        } else {
            AllowedGates::Private
        };
        let mut load_ctx = LoadCtx::default();

        let extra_load_ids: Vec<_> = extras
            .into_iter()
            .map(|extra| {
                if mondebug {
                    tracing::info!("loading ld preload library: {}", extra.name);
                } else {
                    tracing::debug!("loading ld preload library: {}", extra.name);
                }
                dynlink.load_library_in_compartment(
                    root_comp_id,
                    extra.clone(),
                    AllowedGates::Private,
                    &mut load_ctx,
                )
            })
            .try_collect()?;

        let mut loads = UnloadOnDrop(dynlink.load_library_in_compartment(
            root_comp_id,
            root_unlib.clone(),
            allowed_gates,
            &mut load_ctx,
        )?);

        for extra in &extra_load_ids {
            for extra in extra {
                loads.0.push(extra.clone());
            }
        }

        // The dynamic linker gives us a list of loaded libraries, and which compartments they ended
        // up in. For each of those, we may need to inject the runtime library. Collect all
        // the information about the extra compartments.
        let mut cache = HashSet::new();
        let extra_compartments = loads.0.iter().filter_map(|load| {
            if load.comp != root_comp_id {
                // This compartment was loaded in addition to the root comp as part of our
                // initial load request. Check if we haven't seen it before.
                if cache.contains(&load.comp) {
                    return None;
                }
                cache.insert(load.comp);

                // Inject the runtime library, careful to collect the error and keep going.
                let rt_id =
                    match Self::maybe_inject_runtime(dynlink, load.lib, load.comp, &mut load_ctx) {
                        Ok(id) => id,
                        Err(e) => return Some(Err(e)),
                    };
                Some(LoadInfo::new(
                    dynlink,
                    load.lib,
                    rt_id,
                    *load_ctx.set.get(&load.comp).unwrap(),
                    false,
                    &[],
                ))
            } else {
                None
            }
        });

        let extra_compartments = DynlinkError::collect(
            dynlink::DynlinkErrorKind::CompartmentLoadFail {
                compartment: comp_name.to_string(),
            },
            extra_compartments,
        )?;

        let root_id = loads.0[0].lib;
        let rt_id = Self::maybe_inject_runtime(dynlink, root_id, root_comp_id, &mut load_ctx)?;
        let extra_lids = extra_load_ids
            .iter()
            .flatten()
            .map(|x| x.lib)
            .collect::<Vec<_>>();
        for extra in &extra_lids {
            dynlink.relocate_all(*extra)?;
        }
        dynlink.relocate_all(root_id)?;

        let is_binary = dynlink.get_library(root_id)?.is_binary();
        let root_comp = LoadInfo::new(
            dynlink,
            root_id,
            rt_id,
            *load_ctx.set.get(&root_comp_id).unwrap(),
            is_binary,
            extra_lids.as_slice(),
        )?;

        if mondebug {
            let print_comp = |cmp: &LoadInfo| -> miette::Result<()> {
                let dcmp = dynlink.get_compartment(cmp.comp_id)?;
                tracing::info!("Loaded libraries for {}:", &dcmp.name);
                for lid in dcmp.library_ids() {
                    let lib = dynlink.get_library(lid)?;
                    let mut flags = ["-", "-", "-"];
                    if lib.is_binary() {
                        flags[0] = "B";
                    } else {
                        flags[0] = "l";
                    }
                    if lib.id() == cmp.rt_id {
                        flags[1] = "r";
                    } else if lib.id() == cmp.root_id {
                        flags[1] = "R";
                    }
                    if lib.allows_gates() {
                        flags[2] = "g";
                    }
                    let flags = flags.join("");
                    tracing::info!("{:16x} {} {}", lib.base_addr(), flags, &lib.name);
                    if let Some(isg) = lib.iter_secgates() {
                        for gate in isg {
                            tracing::info!(
                                "    GATE {:16x} {}",
                                gate.imp,
                                gate.name().to_string_lossy()
                            )
                        }
                    }
                }
                Ok(())
            };
            tracing::info!("Load info for {}", comp_name);
            let _ = print_comp(&root_comp);
            for cmp in &extra_compartments {
                let _ = print_comp(cmp);
            }
        }

        // We don't want to drop anymore, since now drop-cleanup will be handled by RunCompLoader.
        std::mem::forget(loads);
        Ok(RunCompLoader {
            loaded_extras: extra_compartments,
            root_comp,
        })
    }

    pub fn build_rcs(
        self,
        cmp: &mut CompartmentMgr,
        dynlink: &mut Context,
        _mondebug: bool,
    ) -> miette::Result<ObjID> {
        let make_new_handle = |id| {
            Space::safe_create_and_map_runtime_object(
                &get_monitor().space,
                id,
                MapFlags::READ | MapFlags::WRITE,
            )
        };

        let root_rc = self.root_comp.build_runcomp(
            make_new_handle(self.root_comp.sctx_id)?,
            StackObject::new(make_new_handle(self.root_comp.sctx_id)?, DEFAULT_STACK_SIZE)?,
        )?;

        let mut ids = vec![root_rc.instance];
        // Make all the handles first, for easier cleanup.
        let handles = self
            .loaded_extras
            .iter()
            .map(|extra| {
                Ok::<_, miette::Report>((
                    make_new_handle(extra.sctx_id)?,
                    StackObject::new(make_new_handle(extra.sctx_id)?, DEFAULT_STACK_SIZE)?,
                ))
            })
            .try_collect::<Vec<_>>()?;
        // Construct the RunComps for all the extra compartments.
        let mut extras = self
            .loaded_extras
            .iter()
            .zip(handles)
            .map(|extra| extra.0.build_runcomp(extra.1 .0, extra.1 .1))
            .try_collect::<Vec<_>>()?;

        for rc in extras.drain(..) {
            ids.push(rc.instance);
            cmp.insert(rc);
        }
        cmp.insert(root_rc);
        std::mem::forget(self);

        // Set all the dependency information.
        for id in &ids {
            let Ok(comp) = cmp.get(*id) else { continue };
            let mut deps = dynlink
                .compartment_dependencies(comp.compartment_id)?
                .iter()
                .filter_map(|item| cmp.get_dynlinkid(*item).map(|rc| rc.instance).ok())
                .collect();
            cmp.get_mut(*id).unwrap().deps.append(&mut deps);

            let Ok(comp) = cmp.get(*id) else { continue };
            tracing::debug!("set comp {} deps to {:?}", comp.name, comp.deps);
        }
        Self::rec_inc_all_use_counts(cmp, ids[0], &HashSet::from_iter(ids.iter().cloned()));

        Ok(ids[0])
    }

    fn rec_inc_all_use_counts(
        cmgr: &mut CompartmentMgr,
        start: ObjID,
        created: &HashSet<ObjID>,
    ) -> Option<()> {
        debug_assert!(created.contains(&start));
        let rc = cmgr.get(start).ok()?;
        for dep in rc.deps.clone() {
            if created.contains(&dep) {
                Self::rec_inc_all_use_counts(cmgr, dep, created);
            }
            if let Ok(rc) = cmgr.get_mut(dep) {
                rc.inc_use_count();
            }
        }

        Some(())
    }
}

impl Monitor {
    pub(crate) fn start_compartment(
        &self,
        instance: ObjID,
        args: &[&CStr],
        env: &[&CStr],
        mondebug: bool,
    ) -> Result<(), TwzError> {
        if mondebug {
            tracing::info!("start compartment {}: {:?} {:?}", instance, args, env);
        }
        let deps = {
            let cmp = self.comp_mgr.read(ThreadKey::get().unwrap());
            let rc = cmp.get(instance)?;
            tracing::debug!(
                "starting compartment {} ({}) (binary = {})",
                rc.name,
                rc.instance,
                rc.has_flag(COMP_IS_BINARY)
            );
            rc.deps.clone()
        };
        for dep in deps {
            self.start_compartment(dep, &[], env, false)?;
        }
        // Check the state of this compartment.
        let state = self.load_compartment_flags(instance);
        if state & COMP_EXITED != 0 || state & COMP_DESTRUCTED != 0 {
            tracing::error!(
                "tried to start compartment ({:?}, {}) that has already exited (state: {:x})",
                self.comp_name(instance),
                instance,
                state
            );
            return Err(GenericError::Internal.into());
        }

        loop {
            // Check the state of this compartment.
            let state = self.load_compartment_flags(instance);
            if state & COMP_READY != 0 {
                return Ok(());
            }
            let info = {
                let (ref mut tmgr, ref mut cmp, ref mut dynlink, _, _) =
                    *self.locks.lock(ThreadKey::get().unwrap());
                let rc = cmp.get_mut(instance)?;

                rc.start_main_thread(state, &mut *tmgr, &mut *dynlink, args, env)
            };
            if info.is_none() {
                return Err(GenericError::Internal.into());
            }
            self.wait_for_compartment_state_change(instance, state);
        }
    }
}
