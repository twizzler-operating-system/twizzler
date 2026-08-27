use std::{alloc::Layout, collections::HashSet, ffi::CStr, ptr::null_mut, time::Instant};

use dynlink::{
    compartment::CompartmentId,
    context::{Context, LoadIds, NewCompartmentFlags},
    engines::LoadCtx,
    library::{AllowedGates, LibraryId, UnloadedLibrary},
    DynlinkError, SMALL_STRING_SIZE, SMALL_VEC_SIZE,
};
use happylock::ThreadKey;
use monitor_api::SharedCompConfig;
use smallstr::SmallString;
use talc::{ErrOnOom, Talc};
use tinyvec::TinyVec;
use twizzler_abi::write_note;
use twizzler_rt_abi::{
    bindings::binding_info,
    core::{CtorSet, RuntimeInfo},
    error::{GenericError, TwzError},
    object::{MapFlags, ObjID},
};

use super::{
    CompConfigObject, CompartmentMgr, RunComp, StackObject, COMP_DESTRUCTED, COMP_EXITED,
    COMP_IS_BINARY, COMP_READY, COMP_STARTED,
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
    loaded_extras: dynlink::Vec<LoadInfo, SMALL_VEC_SIZE>,
    root_comp: LoadInfo,
}

struct UnloadOnDrop(TinyVec<[LoadIds; SMALL_VEC_SIZE]>);

impl Drop for UnloadOnDrop {
    fn drop(&mut self) {
        tracing::warn!("todo: drop");
    }
}

/// A compartment load that has mutated the dynlink graph but has not relocated yet.
///
/// Splitting the load here is the point: graph mutation needs the dynlink write lock, relocation
/// does not (it writes to segment memory and to each library's atomic `reloc_state`). The caller
/// drops the write lock and re-takes a read lock between the two halves, so the relocation phase --
/// a median 6.7 ms of a 31 ms load -- no longer blocks `lookup_symbol`, `get_library_info`, and the
/// compartment getters.
pub struct PendingCompLoad {
    loads: UnloadOnDrop,
    loaded_extras: dynlink::Vec<LoadInfo, SMALL_VEC_SIZE>,
    extra_lids: Vec<LibraryId>,
    root_id: LibraryId,
    rt_id: LibraryId,
    root_sctx: ObjID,
    _start_1: Instant,
    _start_2: Instant,
    _t_preloads: u64,
    _t_root: u64,
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
    name: SmallString<[u8; SMALL_STRING_SIZE]>,
    comp_id: CompartmentId,
    // all constructors for all libraries
    ctor_info: dynlink::Vec<CtorSet, SMALL_VEC_SIZE>,
    // entry point to call for the runtime to init this compartment
    entry: Option<extern "C" fn(*const RuntimeInfo) -> !>,
    // entry point for the runtime to call to start the program
    main_entry: Option<extern "C" fn(*const RuntimeInfo) -> !>,
    is_binary: bool,
}

impl Default for LoadInfo {
    fn default() -> Self {
        Self {
            root_id: LibraryId::default(),
            rt_id: LibraryId::default(),
            sctx_id: 0.into(),
            name: "".into(),
            comp_id: CompartmentId::default(),
            ctor_info: dynlink::Vec::new(),
            entry: None,
            main_entry: None,
            is_binary: false,
        }
    }
}

impl LoadInfo {
    fn new(
        dynlink: &Context,
        root_id: LibraryId,
        rt_id: LibraryId,
        sctx_id: ObjID,
        is_binary: bool,
        extras: &[LibraryId],
    ) -> Result<Self, DynlinkError> {
        let root_lib = dynlink.get_library(root_id)?;
        let lib = dynlink.get_library(rt_id)?;
        let extra_ctors: Vec<_> = extras
            .iter()
            .map(|extra| dynlink.build_ctors_list::<1>(*extra, Some(lib.compartment()), None))
            .try_collect()?;
        let root_ctors = dynlink.build_ctors_list::<1>(root_id, Some(lib.compartment()), None)?;
        let mut ctor_info: dynlink::Vec<_, SMALL_VEC_SIZE> =
            extra_ctors.iter().flatten().copied().collect();
        ctor_info.extend_from_slice(root_ctors.as_slice());

        let main_entry = root_lib.get_entry_address().ok();

        Ok(Self {
            root_id,
            rt_id,
            comp_id: lib.compartment(),
            sctx_id,
            name: dynlink
                .get_compartment(lib.compartment())?
                .name
                .as_str()
                .into(),
            ctor_info,
            entry: Some(lib.get_entry_address()?),
            main_entry,
            is_binary,
        })
    }

    fn build_runcomp(
        &self,
        handle: MapHandle,
        stack_object: StackObject,
        is_debugging: bool,
        controller: Option<ObjID>,
        mut loader_config: monitor_api::CompartmentLoaderConfig,
    ) -> Result<RunComp, DynlinkError> {
        let _start = Instant::now();
        let mut comp_config = CompConfigObject::new(
            handle,
            SharedCompConfig::new(self.sctx_id, null_mut(), loader_config),
        );

        let flags = if self.is_binary { COMP_IS_BINARY } else { 0 };

        let mut alloc = Talc::new(ErrOnOom);
        unsafe { alloc.claim(comp_config.alloc_span()).unwrap() };

        // Both of these arrive pointing into the *loading* compartment's memory, which means
        // nothing in the new one. Copy each into the compartment's own allocation and rewrite the
        // pointer, then write the config back once, after every rewrite -- writing it inside the
        // first branch would publish a config still carrying the second stale pointer.
        let mut rewrote = false;

        if !loader_config.fd_spec.is_null() {
            let fd_spec_layout = Layout::array::<binding_info>(loader_config.fd_spec_len).unwrap();
            let fd_spec_ptr =
                unsafe { alloc.malloc(fd_spec_layout).unwrap().cast::<binding_info>() };
            let fd_spec_slice = unsafe {
                core::slice::from_raw_parts_mut(fd_spec_ptr.as_ptr(), loader_config.fd_spec_len)
            };
            let src_fd_spec_slice = unsafe {
                core::slice::from_raw_parts(loader_config.fd_spec, loader_config.fd_spec_len)
            };
            fd_spec_slice.copy_from_slice(src_fd_spec_slice);
            loader_config.fd_spec = fd_spec_ptr.as_ptr();
            rewrote = true;
        }

        if !loader_config.initial_cwd.is_null() && loader_config.initial_cwd_len > 0 {
            let len = loader_config.initial_cwd_len;
            // A path we cannot place is not worth failing a compartment load over: drop the
            // inheritance and let it start at the root, rather than panicking the monitor.
            match Layout::array::<u8>(len)
                .ok()
                .and_then(|layout| unsafe { alloc.malloc(layout).ok() })
            {
                Some(ptr) => {
                    let dst = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) };
                    let src = unsafe { core::slice::from_raw_parts(loader_config.initial_cwd, len) };
                    dst.copy_from_slice(src);
                    loader_config.initial_cwd = ptr.as_ptr();
                }
                None => {
                    tracing::warn!("no room for initial cwd in compartment config; starting at /");
                    loader_config.initial_cwd = core::ptr::null();
                    loader_config.initial_cwd_len = 0;
                }
            }
            rewrote = true;
        }

        if rewrote {
            comp_config.write_config(SharedCompConfig::new(
                self.sctx_id,
                null_mut(),
                loader_config,
            ));
        }
        tracing::trace!("build_runcomp in {}ms", _start.elapsed().as_millis());

        Ok(RunComp::new(
            self.sctx_id,
            self.sctx_id,
            self.name.to_string(),
            self.comp_id,
            vec![],
            comp_config,
            flags,
            stack_object,
            self.entry.map(|x| x as usize).unwrap_or_default(),
            self.main_entry.map(|x| x as usize).unwrap_or_default(),
            &self.ctor_info,
            is_debugging,
            controller,
            alloc,
        ))
    }
}

impl Drop for RunCompLoader {
    fn drop(&mut self) {
        tracing::warn!("drop RunCompLoader: TODO");
    }
}

const RUNTIME_NAME: &str = "libtwz_rt.so";

/// Switch for the per-load phase counter (`LOADPHAS`): total / preloads / root load / relocate.
///
/// Answered round 8's question -- a compartment load is a median 31 ms, of which the root library's
/// `load_library_in_compartment` is 24 ms and relocation 6.7 -- so it is off. Left in place because
/// the next attempt on that 24 ms needs it back.
const LOAD_PHASE_STATS: bool = false;

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

    /// Load libraries into the dynamic linker, without relocating them.
    ///
    /// This is the half of a compartment load that mutates the dynlink graph, and is the only half
    /// that needs the write lock. Finish with [`PendingCompLoad::relocate_and_finish`] under a read
    /// lock.
    pub fn load_graph(
        dynlink: &mut Context,
        comp_name: &str,
        root_unlib: UnloadedLibrary,
        extras: &[UnloadedLibrary],
        extras_sctx: &[UnloadedLibrary],
        new_comp_flags: NewCompartmentFlags,
        mondebug: bool,
    ) -> miette::Result<PendingCompLoad> {
        let _start_1 = Instant::now();
        let root_comp_id = dynlink.add_compartment(comp_name, new_comp_flags)?;
        let allowed_gates = if new_comp_flags.contains(NewCompartmentFlags::EXPORT_GATES) {
            AllowedGates::Public
        } else {
            AllowedGates::Private
        };
        let mut load_ctx = LoadCtx::default();
        let _start_2 = Instant::now();

        let mut extra_sctx_load_ids: Vec<_> = extras_sctx
            .into_iter()
            .map(|extra| {
                let comp_id = dynlink
                    .add_compartment(extra.name.clone(), NewCompartmentFlags::EXPORT_GATES)?;
                if mondebug {
                    tracing::info!(
                        "loading sctx preload library: {} -> {}",
                        extra.name,
                        comp_id
                    );
                } else {
                    tracing::debug!(
                        "loading sctx preload library: {} -> {}",
                        extra.name,
                        comp_id
                    );
                }
                dynlink.load_library_in_compartment(
                    comp_id,
                    extra.clone(),
                    AllowedGates::Public,
                    &mut load_ctx,
                )
            })
            .try_collect()?;

        let mut extra_load_ids: Vec<_> = extras
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

        let _t_preloads = _start_2.elapsed().as_nanos() as u64;
        let _t_root_start = Instant::now();
        let mut loads = UnloadOnDrop(dynlink.load_library_in_compartment(
            root_comp_id,
            root_unlib.clone(),
            allowed_gates,
            &mut load_ctx,
        )?);
        let _t_root = _t_root_start.elapsed().as_nanos() as u64;

        extra_load_ids.append(&mut extra_sctx_load_ids);

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
            tracing::debug!(
                "extra? {} {} {}",
                load.comp,
                root_comp_id,
                cache.contains(&load.comp)
            );
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
                compartment: comp_name.into(),
            },
            extra_compartments,
        )?;
        tracing::trace!("extras: {:?}", extra_compartments);

        let root_id = loads.0[0].lib;
        let rt_id = Self::maybe_inject_runtime(dynlink, root_id, root_comp_id, &mut load_ctx)?;
        let extra_lids = extra_load_ids
            .iter()
            .flatten()
            .map(|x| x.lib)
            .collect::<Vec<_>>();

        Ok(PendingCompLoad {
            loads,
            loaded_extras: extra_compartments,
            extra_lids,
            root_id,
            rt_id,
            root_sctx: *load_ctx.set.get(&root_comp_id).unwrap(),
            _start_1,
            _start_2,
            _t_preloads,
            _t_root,
        })
    }
}

impl PendingCompLoad {
    /// Relocate the loaded libraries and finish building the [`RunCompLoader`].
    ///
    /// Takes `&Context`: relocation mutates segment memory and each library's atomic relocation
    /// state, not the graph. A concurrent reader can see these libraries in the graph before this
    /// returns, which is what `Library::is_relocated` guards.
    pub fn relocate_and_finish(
        self,
        dynlink: &Context,
        comp_name: &str,
        mondebug: bool,
    ) -> miette::Result<RunCompLoader> {
        let PendingCompLoad {
            loads,
            loaded_extras: extra_compartments,
            extra_lids,
            root_id,
            rt_id,
            root_sctx,
            _start_1,
            _start_2,
            _t_preloads,
            _t_root,
        } = self;

        let _start_3 = Instant::now();
        for extra in &extra_lids {
            dynlink.relocate_all(*extra)?;
        }
        dynlink.relocate_all(root_id)?;

        let _t_reloc = _start_3.elapsed().as_nanos() as u64;
        // Where a compartment load's 20-100 ms actually goes (`sysperf.md` round 8). Recorded per
        // load, deferred through statlog: this runs with the dynlink write lock held, so a console
        // write here would be charged to the hold being investigated.
        secgate::statlog::record_on(
            LOAD_PHASE_STATS,
            "LOADPHAS",
            _start_1.elapsed().as_nanos() as u64 / 1000,
            &[
                _t_preloads / 1000,
                _t_root / 1000,
                _t_reloc / 1000,
                extra_lids.len() as u64,
            ],
        );
        let is_binary = dynlink.get_library(root_id)?.is_binary();
        let root_comp = LoadInfo::new(
            dynlink,
            root_id,
            rt_id,
            root_sctx,
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

        // Microseconds, not millis: individual phases are frequently sub-millisecond.
        tracing::debug!(
            "COMPLOAD {}: prepped {}us, loaded {}us, relocated {}us",
            comp_name,
            (_start_2 - _start_1).as_micros(),
            (_start_3 - _start_2).as_micros(),
            _start_3.elapsed().as_micros()
        );

        Ok(RunCompLoader {
            loaded_extras: extra_compartments,
            root_comp,
        })
    }
}

impl RunCompLoader {
    pub fn build_rcs(
        self,
        cmp: &mut CompartmentMgr,
        dynlink: &mut Context,
        mondebug: bool,
        is_debugging: bool,
        controller: Option<ObjID>,
        loader_config: monitor_api::CompartmentLoaderConfig,
    ) -> miette::Result<ObjID> {
        let make_new_handle = |ty, id, name| {
            if mondebug {
                tracing::info!(
                    "creating runtime {} object {} for compartment {}",
                    ty,
                    id,
                    name
                );
            }
            let handle = Space::safe_create_and_map_runtime_object(
                &get_monitor().space,
                id,
                MapFlags::READ | MapFlags::WRITE,
            );
            if let Ok(ref handle) = handle {
                write_note!(handle.id(), "{}:{}", ty, name);
            }
            handle
        };
        let stack = StackObject::new(
            make_new_handle("stack", self.root_comp.sctx_id, &self.root_comp.name)?,
            DEFAULT_STACK_SIZE,
        )?;

        let mut root_rc = self.root_comp.build_runcomp(
            make_new_handle("comp-config", self.root_comp.sctx_id, &self.root_comp.name)?,
            stack,
            is_debugging,
            controller,
            loader_config,
        )?;
        tracing::trace!("starting {} as {}", self.root_comp.name, root_rc.instance);

        let mut ids = vec![root_rc.instance];
        // Make all the handles first, for easier cleanup.
        let handles = self
            .loaded_extras
            .iter()
            .map(|extra| {
                let stack = StackObject::new(
                    make_new_handle("stack", extra.sctx_id, &extra.name)?,
                    DEFAULT_STACK_SIZE,
                )?;
                Ok::<_, miette::Report>((
                    make_new_handle("comp-config", extra.sctx_id, &extra.name)?,
                    stack,
                ))
            })
            .try_collect::<Vec<_>>()?;
        // Construct the RunComps for all the extra compartments.
        let mut extras = self
            .loaded_extras
            .iter()
            .zip(handles)
            .map(|extra| {
                extra
                    .0
                    .build_runcomp(extra.1 .0, extra.1 .1, false, controller, loader_config)
            })
            .try_collect::<Vec<_>>()?;

        for rc in extras.drain(..) {
            ids.push(rc.instance);
            root_rc.deps.push(rc.instance);
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
        suspend_on_start: bool,
    ) -> Result<(), TwzError> {
        let deps = {
            // Site 4 of the spawn-side lock-wait probe; see `load_compartment`'s `LCKWAIT`.
            let _t = crate::mon::compartment::SPAWN_PHASE_STATS.then(Instant::now);
            let cmp = crate::lockdiag::watched(self.comp_mgr.read(ThreadKey::get().unwrap()));
            if let Some(t) = _t {
                secgate::statlog::record_on(
                    crate::mon::compartment::SPAWN_PHASE_STATS,
                    "LCKWAIT",
                    t.elapsed().as_micros() as u64,
                    &[4],
                );
            }
            let rc = cmp.get(instance)?;

            if mondebug {
                tracing::info!(
                    "start compartment {}: {:?} {:?} flags = {:x}",
                    instance,
                    args,
                    env,
                    rc.raw_flags()
                );
            }

            tracing::debug!(
                "starting compartment {} ({}) (binary = {})",
                rc.name,
                rc.instance,
                rc.has_flag(COMP_IS_BINARY)
            );
            rc.deps.clone()
        };
        for dep in deps {
            self.start_compartment(dep, &[], env, false, false)?;
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

        let _loop_start = Instant::now();
        let mut _smt = core::time::Duration::ZERO;
        loop {
            // Check the state of this compartment.
            let state = self.load_compartment_flags(instance);
            if state & COMP_READY != 0 {
                tracing::trace!(
                    "started main detected ready in {}ms",
                    _loop_start.elapsed().as_millis()
                );
                // Splits the `start` phase: how much is the monitor and kernel starting a thread,
                // and how much is then waiting for the child's own runtime to come up. They want
                // different fixes, and SPAWNPHA cannot tell them apart.
                secgate::statlog::record_on(
                    crate::mon::compartment::SPAWN_PHASE_STATS,
                    "STARTSPL",
                    _loop_start.elapsed().as_micros() as u64,
                    &[_smt.as_micros() as u64],
                );
                return Ok(());
            }
            if suspend_on_start {
                // We can't wait for ready, since that need the thread to run.
                if state & COMP_STARTED != 0 {
                    tracing::trace!(
                        "started main detected started in {}ms",
                        _loop_start.elapsed().as_millis()
                    );
                    return Ok(());
                }
            }
            let info = {
                // Site 5 of the spawn-side lock-wait probe.
                let _t = crate::mon::compartment::SPAWN_PHASE_STATS.then(Instant::now);
                let (ref mut tmgr, ref mut cmp, ref mut dynlink, _, _) =
                    *crate::lockdiag::watched(self.locks.lock(ThreadKey::get().unwrap()));
                if let Some(t) = _t {
                    secgate::statlog::record_on(
                        crate::mon::compartment::SPAWN_PHASE_STATS,
                        "LCKWAIT",
                        t.elapsed().as_micros() as u64,
                        &[5],
                    );
                }
                let rc = cmp.get_mut(instance)?;

                let _start = Instant::now();
                let r = rc.start_main_thread(
                    state,
                    &mut *tmgr,
                    &mut *dynlink,
                    args,
                    env,
                    suspend_on_start,
                );
                _smt += _start.elapsed();
                tracing::trace!("start_main_thread in {}ms", _start.elapsed().as_millis());

                r
            };
            if info.is_none() {
                return Err(GenericError::Internal.into());
            }
            self.wait_for_compartment_state_change(instance, state);
        }
    }
}
