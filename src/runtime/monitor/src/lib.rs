#![feature(naked_functions)]

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
mod state;

pub mod secgate_test;

pub fn main() {
    std::env::set_var("RUST_BACKTRACE", "full");
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .with_target(false)
        .with_span_events(FmtSpan::ACTIVE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

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
    main_thread.join().unwrap();
    warn!("monitor main thread exited");
}

fn monitor_init(state: Arc<Mutex<MonitorState>>) {
    info!("monitor early init completed, starting init");

    let lib = dynlink::library::UnloadedLibrary::new("libhello_world.so");

    let mut state = state.lock().unwrap();
    let _ = state.dynlink.add_compartment("test").unwrap();

    let _ = state
        .dynlink
        .load_library_in_compartment("test", lib, |mut name| {
            if name.starts_with("libstd-") {
                name = "libstd.so";
            }
            let id = find_init_name(name)?;
            let obj = twizzler_runtime_api::get_runtime()
                .map_object(id.as_u128(), twizzler_runtime_api::MapFlags::READ)
                .ok()?;
            Some(Backing::new(obj))
        })
        .unwrap();

    let comp = state.dynlink.get_compartment("test").unwrap();
    state
        .dynlink
        .relocate_all(comp, "libhello_world.so")
        .unwrap();

    info!("lookup entry");

    let hwlib = state
        .dynlink
        .lookup_loaded_library(comp, "libhello_world.so")
        .unwrap();
    let sym = state
        .dynlink
        .lookup_symbol(&hwlib, "test_sec_call", LookupFlags::empty())
        .unwrap();

    let addr = sym.reloc_value();
    info!("addr = {:x}", addr);
    let ptr: extern "C" fn() = unsafe { core::mem::transmute(addr as usize) };
    (ptr)();
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
