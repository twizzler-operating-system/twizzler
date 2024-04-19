#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(c_str_literals)]
#![feature(new_uninit)]

use std::sync::{Arc, Mutex};

use dynlink::{engines::Backing, symbol::LookupFlags};
use state::MonitorState;
use tracing::{debug, info, trace, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_create, ObjectCreate},
};
use twizzler_object::ObjID;
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

    set_upcall_handler(&crate::upcall::upcall_monitor_handler).unwrap();
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

fn load_hello_world_test(state: &Arc<Mutex<MonitorState>>) -> miette::Result<()> {
    let lib = dynlink::library::UnloadedLibrary::new("hello-world");
    let rt_lib = dynlink::library::UnloadedLibrary::new("libtwz_rt.so");

    let mut state = state.lock().unwrap();
    let test_comp_id = state.dynlink.add_compartment("test")?;

    let libhw_id =
        state
            .dynlink
            .load_library_in_compartment(test_comp_id, lib, bootstrap_name_res)?;

    let rt_id =
        match state
            .dynlink
            .load_library_in_compartment(test_comp_id, rt_lib, bootstrap_name_res)
        {
            Ok(rt_id) => {
                state.dynlink.add_manual_dependency(libhw_id, rt_id);
                rt_id
            }
            Err(_) => state
                .dynlink
                .lookup_library(test_comp_id, "libtwz_rt.so")
                .unwrap(),
        };

    println!("found rt_id: {}", rt_id);
    let rt_lib = state.dynlink.get_library(rt_id).unwrap();

    drop(rt_lib);

    state.dynlink.relocate_all(libhw_id)?;

    let test_comp = Comp::new(
        1.into(),
        state.dynlink.get_compartment_mut(test_comp_id).unwrap(),
    )
    .unwrap();

    info!("!! root = {}", libhw_id);
    let ctors = state.dynlink.build_ctors_list(libhw_id).unwrap();

    let rtinfo = CompartmentInitInfo {
        ctor_array_start: ctors.as_ptr() as usize,
        ctor_array_len: ctors.len(),
        comp_config_addr: test_comp.get_comp_config() as *const _ as usize,
    };
    state.add_comp(test_comp, libhw_id.into());

    info!("lookup entry");

    let rt_lib = state.dynlink.get_library(rt_id).unwrap();
    let entry = rt_lib.get_entry_address().unwrap();

    let aux = [
        AuxEntry::RuntimeInfo(&rtinfo as *const _ as usize, 1),
        AuxEntry::Null,
    ];
    println!("==> {:p}", entry);
    drop(state);
    entry(aux.as_ptr());
    /*
    let sym = state
        .dynlink
        .lookup_symbol(libhw_id, "test_sec_call", LookupFlags::empty())?;

    let addr = sym.reloc_value();
    info!("addr = {:x}", addr);
    let ptr: extern "C" fn() = unsafe { core::mem::transmute(addr as usize) };
    (ptr)();
    */

    Ok(())
}

fn bootstrap_name_res(mut name: &str) -> Option<Backing> {
    if name.starts_with("libstd-") {
        name = "libstd.so";
    }
    let id = find_init_name(name)?;
    let obj = twizzler_runtime_api::get_runtime()
        .map_object(id, twizzler_runtime_api::MapFlags::READ)
        .ok()?;
    Some(Backing::new(obj))
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
