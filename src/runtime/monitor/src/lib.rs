#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(c_str_literals)]
#![feature(new_uninit)]

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use dynlink::{
    compartment::CompartmentId,
    context::engine::{ContextEngine, Selector},
    engines::Engine,
    library::{LibraryId, UnloadedLibrary},
};
use state::{MonitorState, MonitorStateRef};
use tracing::{debug, info, trace, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twizzler_abi::object::ObjID;
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_runtime_api::AuxEntry;
use twz_rt::{set_upcall_handler, CompartmentInitInfo};

use crate::{compartment::Comp, state::set_monitor_state};

mod compartment;
mod init;
mod object;
mod runtime;
pub mod secgate_test;
mod state;
mod thread;
mod upcall;

mod api;
mod compman;
mod mapman;

#[path = "../secapi/gates.rs"]
mod gates;

pub fn main() {
    std::env::set_var("RUST_BACKTRACE", "full");
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .with_target(false)
        .with_span_events(FmtSpan::ACTIVE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    miette::set_hook(Box::new(|_| {
        Box::new(miette::NarratableReportHandler::new().with_cause_chain())
    }))
    .unwrap();

    trace!("monitor entered, discovering dynlink context");
    let init =
        init::bootstrap_dynlink_context().expect("failed to discover initial dynlink context");
    let mut state = state::MonitorState::new(init);

    let monitor_comp_id = state.dynlink.lookup_compartment("monitor").unwrap();
    let monitor_comp = Comp::new(
        0.into(),
        state.dynlink.get_compartment_mut(monitor_comp_id).unwrap(),
    )
    .unwrap();
    state.add_comp(monitor_comp, twizzler_runtime_api::LibraryId(0));

    let state = Arc::new(Mutex::new(state));
    tracing::info!(".. state: {:p}", state);
    debug!(
        "found dynlink context, with root {}",
        state.lock().unwrap().root
    );

    std::env::set_var("RUST_BACKTRACE", "1");

    set_upcall_handler(&crate::upcall::upcall_monitor_handler);
    set_monitor_state(state.clone());

    let main_thread = std::thread::spawn(|| monitor_init(state));
    let _r = main_thread.join().unwrap().map_err(|e| {
        tracing::error!("{:?}", e);
    });
    warn!("monitor main thread exited");
}

fn monitor_init(state: Arc<Mutex<MonitorState>>) -> miette::Result<()> {
    info!("monitor early init completed, starting init");

    {
        let state = state.lock().unwrap();
        let comp = state.dynlink.lookup_compartment("monitor").unwrap();
        let mon = state.dynlink.lookup_library(comp, "libmonitor.so").unwrap();

        let mon = state.dynlink.get_library(mon)?;

        for gate in mon.iter_secgates().unwrap() {
            let name = gate.name().to_string_lossy();
            info!("secure gate in {} => {}: {:x}", mon.name, name, gate.imp);
        }
    }

    load_hello_world_test(&state).unwrap();

    Ok(())
}

struct Sel;

impl Selector<Engine> for Sel {
    fn resolve_name(&self, mut name: &str) -> Option<<Engine as ContextEngine>::Backing> {
        if name.starts_with("libstd-") {
            name = "libstd.so";
        }
        let id = find_init_name(name)?;
        let obj = twizzler_runtime_api::get_runtime()
            .map_object(id.as_u128(), twizzler_runtime_api::MapFlags::READ)
            .ok()?;
        Some(<Engine as ContextEngine>::Backing::new(obj))
    }
}

const RUNTIME_NAME: &'static str = "libtwz_rt.so";
static CTX_NUM: AtomicU64 = AtomicU64::new(1);

struct ExtraCompInfo {
    pub root_id: LibraryId,
    pub rt_id: LibraryId,
    pub sctx_id: ObjID,
}

struct Loader {
    state: MonitorStateRef,
    extra_compartments: Vec<ExtraCompInfo>,
    start_unload: Option<LibraryId>,
}

impl Drop for Loader {
    fn drop(&mut self) {
        if let Some(start_unload) = self.start_unload.take() {
            if let Ok(state) = self.state.lock() {
                tracing::warn!("TODO: unload library");
            }
        }
        while let Some(extra) = self.extra_compartments.pop() {
            if let Ok(state) = self.state.lock() {
                tracing::warn!("TODO: unload compartment");
            }
        }
    }
}

impl Loader {
    fn new(state: Arc<Mutex<MonitorState>>) -> Self {
        Self {
            state,
            extra_compartments: vec![],
            start_unload: None,
        }
    }

    fn clear(&mut self) {
        self.start_unload = None;
        self.extra_compartments.clear();
    }

    fn start_thread_in_compartment(
        state: &MonitorStateRef,
        rt_id: LibraryId,
        sctx_id: ObjID,
        rt_info: CompartmentInitInfo,
    ) -> miette::Result<()> {
        let (entry, rt_info) = {
            let mut state = state.lock().map_err(|_| miette::miette!("poison error"))?;
            let rt_info = state
                .lookup_comp_mut(sctx_id)
                .unwrap()
                .monitor_new(rt_info)
                .unwrap();
            let rt_lib = state.dynlink.get_library(rt_id)?;

            (rt_lib.get_entry_address()?, rt_info.as_ptr() as usize)
        };

        tracing::debug!(
            "spawning thread in compartment {} at {:p} with {:x}",
            sctx_id,
            entry,
            rt_info
        );
        let join = std::thread::spawn(move || {
            let aux = [AuxEntry::RuntimeInfo(rt_info, 1), AuxEntry::Null];
            entry(aux.as_ptr());
        });
        // TODO: timeout?
        join.join().map_err(|_| miette::miette!("join error"))?;
        Ok(())
    }

    fn maybe_inject_rt(
        state: &mut MutexGuard<'_, MonitorState>,
        root_id: LibraryId,
        comp_id: CompartmentId,
    ) -> miette::Result<LibraryId> {
        let rt_unlib = UnloadedLibrary::new(RUNTIME_NAME);

        if let Some(id) = state.dynlink.lookup_library(comp_id, RUNTIME_NAME) {
            return Ok(id);
        }

        let loads = state
            .dynlink
            .load_library_in_compartment(comp_id, rt_unlib, &Sel)?;

        let rt_id = loads[0].lib;
        state.dynlink.add_manual_dependency(root_id, rt_id);
        Ok(rt_id)
    }

    //#[tracing::instrument(skip(self))]
    pub fn run_a_crate(&mut self, name: &str, comp_name: &str) -> miette::Result<()> {
        let root_unlib = UnloadedLibrary::new(name);
        let mut state = self
            .state
            .lock()
            .map_err(|_| miette::miette!("poison error"))?;
        let root_comp_id = state.dynlink.add_compartment(comp_name)?;

        let loads = state
            .dynlink
            .load_library_in_compartment(root_comp_id, root_unlib, &Sel)?;

        tracing::warn!("==> {:#?}", loads);

        let mut cache = HashMap::new();
        self.extra_compartments = loads
            .iter()
            .filter_map(|load| {
                if load.comp != root_comp_id {
                    if let Ok(lib) = state.dynlink.get_library(load.lib) {
                        if cache.contains_key(&load.comp) {
                            tracing::info!(
                                "load alt compartment library {}: {} (existing)",
                                lib,
                                load.comp
                            );
                            return None;
                        }
                        tracing::info!(
                            "load returned alternate compartment for library {}: {}",
                            lib,
                            load.comp
                        );

                        let rt_id =
                            Loader::maybe_inject_rt(&mut state, load.lib, load.comp).ok()?;

                        let sctx_id = (CTX_NUM.fetch_add(1, Ordering::SeqCst) as u128).into();
                        cache.insert(load.comp, sctx_id);
                        let dep_comp = Comp::new(
                            sctx_id,
                            state.dynlink.get_compartment_mut(load.comp).unwrap(),
                        )
                        .unwrap();
                        state.add_comp(dep_comp, load.lib.into());
                        Some(ExtraCompInfo {
                            root_id: load.lib,
                            rt_id,
                            sctx_id,
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        let root_id = loads[0].lib;
        self.start_unload = Some(root_id);
        tracing::info!("loaded {} as {}", name, root_id);

        let rt_id = Loader::maybe_inject_rt(&mut state, root_id, root_comp_id)?;
        state.dynlink.relocate_all(root_id)?;

        let sctx_id = (CTX_NUM.fetch_add(1, Ordering::SeqCst) as u128).into();
        let root_comp =
            Comp::new(sctx_id, state.dynlink.get_compartment_mut(root_comp_id)?).unwrap();

        let extra_ctors = self
            .extra_compartments
            .iter()
            .filter_map(|extra_info| {
                let comp = state.lookup_comp(extra_info.sctx_id)?;
                Some((
                    state.dynlink.build_ctors_list(extra_info.root_id).ok()?,
                    comp.get_comp_config() as *const _ as usize,
                    extra_info.sctx_id,
                ))
            })
            .collect::<Vec<_>>();

        drop(state);

        for (ctors, comp_config_addr, sctx_id) in extra_ctors {
            let rtinfo = CompartmentInitInfo {
                ctor_array_start: ctors.as_ptr() as usize,
                ctor_array_len: ctors.len(),
                comp_config_addr,
            };
            Loader::start_thread_in_compartment(&self.state, rt_id, sctx_id, rtinfo)?;
        }

        let mut state = self.state.lock().unwrap();

        let ctors = state.dynlink.build_ctors_list(root_id).unwrap();
        // TODO: allocate this in-compartment
        let rtinfo = CompartmentInitInfo {
            ctor_array_start: ctors.as_ptr() as usize,
            ctor_array_len: ctors.len(),
            comp_config_addr: root_comp.get_comp_config() as *const _ as usize,
        };
        state.add_comp(root_comp, root_id.into());

        drop(state);
        tracing::trace!("entry runtime info: {:?}", rtinfo);
        Loader::start_thread_in_compartment(&self.state, rt_id, sctx_id, rtinfo)?;
        Ok(())
    }
}

fn load_hello_world_test(state: &Arc<Mutex<MonitorState>>) -> miette::Result<()> {
    let mut loader = Loader::new(state.clone());
    loader.run_a_crate("hello-world", "test")
}

pub fn get_kernel_init_info() -> &'static KernelInitInfo {
    unsafe {
        (((twizzler_abi::slot::RESERVED_KERNEL_INIT * MAX_SIZE) + NULLPAGE_SIZE)
            as *const KernelInitInfo)
            .as_ref()
            .unwrap()
    }
}

fn find_init_name(name: &str) -> Option<ObjID> {
    let init_info = get_kernel_init_info();
    for n in init_info.names() {
        if n.name() == name {
            return Some(n.id());
        }
    }
    None
}
