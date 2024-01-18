#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(c_str_literals)]

use std::sync::{Arc, Mutex};

use dynlink::{engines::Backing, symbol::LookupFlags};
use state::MonitorState;
use tracing::{debug, info, trace, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_object::ObjID;

use crate::runtime::init_actions;

mod init;
mod runtime;
pub mod secgate_test;
mod state;

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
    let state = Arc::new(Mutex::new(state::MonitorState::new(init)));
    debug!(
        "found dynlink context, with root {}",
        state.lock().unwrap().root
    );

    init_actions(state.clone());
    std::env::set_var("RUST_BACKTRACE", "1");

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

fn load_hello_world_test(state: &Arc<Mutex<MonitorState>>) -> miette::Result<()> {
    let lib = dynlink::library::UnloadedLibrary::new("libhello_world.so");

    let mut state = state.lock().unwrap();
    let test_comp_id = state.dynlink.add_compartment("test")?;
    tracing::info!("here");

    info!("loading hello-world.so");
    let libhw_id = state
        .dynlink
        .load_library_in_compartment(test_comp_id, lib, |mut name| {
            if name.starts_with("libstd-") {
                name = "libstd.so";
            }
            let id = find_init_name(name)?;
            let obj = twizzler_runtime_api::get_runtime()
                .map_object(id.as_u128(), twizzler_runtime_api::MapFlags::READ)
                .ok()?;
            Some(Backing::new(obj))
        })?;

    let _ = state.dynlink.relocate_all(libhw_id)?;
    info!("lookup entry");

    let sym = state
        .dynlink
        .lookup_symbol(libhw_id, "test_sec_call", LookupFlags::empty())?;

    let addr = sym.reloc_value();
    info!("addr = {:x}", addr);
    let ptr: extern "C" fn() = unsafe { core::mem::transmute(addr as usize) };
    (ptr)();

    Ok(())
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
