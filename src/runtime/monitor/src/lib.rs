#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(c_str_literals)]
#![feature(new_uninit)]
#![feature(error_in_core)]
#![feature(hash_extract_if)]
#![feature(offset_of)]

use std::{cmp::Ordering, collections::HashMap, sync::atomic::AtomicU64, time::Duration};

use dynlink::{
    compartment::CompartmentId,
    context::engine::{ContextEngine, Selector},
    engines::Engine,
    library::{LibraryId, UnloadedLibrary},
};
use tracing::{debug, info, trace, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_runtime_api::AuxEntry;
use twz_rt::CompartmentInitInfo;

use crate::{
    api::MONITOR_INSTANCE_ID, compman::COMPMAN, mapman::init_mapping, threadman::THREAD_MGR,
};

mod api;
mod compman;
mod init;
mod mapman;
pub mod secgate_test;
mod threadman;
mod upcall;

#[path = "../secapi/gates.rs"]
mod gates;
//b2130

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

    twz_rt::set_upcall_handler(&crate::upcall::upcall_monitor_handler).unwrap();
    COMPMAN.init(init);
    init_mapping();
    THREAD_MGR.start_cleaner();
    std::env::set_var("RUST_BACKTRACE", "1");

    let mon_rc = COMPMAN.get_comp_inner(MONITOR_INSTANCE_ID).unwrap();
    mon_rc
        .lock()
        .unwrap()
        .start_main_thread(|| {
            // Normally, this main thread would go through and start constructors and stuff.
            // Since we're coming in from a bootstrap, this is already done, so we can just
            // use this as our compartment main, and continue execution from here.
            debug!("monitor main thread continuing");
            let report = monitor_init();
            if let Err(report) = report {
                tracing::error!("{:?}", report);
            }
        })
        .unwrap();

    // TODO: wait for monitor init thread.
    loop {}
}

fn monitor_init() -> miette::Result<()> {
    info!("monitor early init completed, starting init");
    let loader = COMPMAN.load_compartment(
        "test",
        UnloadedLibrary {
            name: "hello-world".to_string(),
        },
    );
    info!("==> {:#?}", loader);
    loader.unwrap().start_main().unwrap();
    Ok(())
}

/*
fn load_hello_world_test(state: &Arc<Mutex<MonitorState>>) -> miette::Result<()> {
    let mut loader = Loader::new(state.clone());
    loader.run_a_crate("hello-world", "test")
}*/

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
