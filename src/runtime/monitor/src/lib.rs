#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(c_str_literals)]
#![feature(new_uninit)]
#![feature(hash_extract_if)]
#![feature(iterator_try_collect)]
#![feature(result_option_inspect)]

use std::mem::ManuallyDrop;

use dynlink::context::NewCompartmentFlags;
use miette::IntoDiagnostic;
use tracing::{debug, info, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twz_rt::{set_upcall_handler, OUR_RUNTIME};

mod compartment;
mod dlengine;
mod init;
mod object;
pub mod secgate_test;
mod upcall;

mod api;
mod mon;

pub use monitor_api::MappedObjectAddrs;

#[path = "../secapi/gates.rs"]
mod gates;

pub fn main() {
    std::env::set_var("RUST_BACKTRACE", "full");
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .with_span_events(FmtSpan::ACTIVE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    miette::set_hook(Box::new(|_| {
        Box::new(miette::NarratableReportHandler::new().with_cause_chain())
    }))
    .unwrap();

    info!("monitor entered, discovering dynlink context");
    let init =
        init::bootstrap_dynlink_context().expect("failed to discover initial dynlink context");

    let mon = mon::Monitor::new(init);
    mon::set_monitor(mon);

    // Safety: the monitor is ready, and so we can set our runtime as ready to use the monitor.
    unsafe { OUR_RUNTIME.set_runtime_ready() };
    // Had to wait till now to be able to spawn threads.
    mon::get_monitor().start_background_threads();

    debug!("Ok, starting monitor main");
    std::env::set_var("RUST_BACKTRACE", "1");
    set_upcall_handler(&crate::upcall::upcall_monitor_handler).unwrap();

    let main_thread = std::thread::spawn(|| monitor_init());
    let _r = main_thread.join().unwrap().map_err(|e| {
        tracing::error!("{:?}", e);
    });
    warn!("monitor main thread exited");
}

#[allow(dead_code, unused_variables, unreachable_code)]
fn monitor_init() -> miette::Result<()> {
    info!("monitor early init completed, starting init");

    info!("starting logboi...");
    let loader =
        monitor_api::CompartmentLoader::new("liblogboi_srv.so", NewCompartmentFlags::EXPORT_GATES);
    let logboi_comp = loader.load().into_diagnostic()?;

    // we want logboi to stick around
    let logboi_comp = ManuallyDrop::new(logboi_comp);

    if false {
        info!("starting global bar");
        let loader =
            monitor_api::CompartmentLoader::new("libbar_srv.so", NewCompartmentFlags::EXPORT_GATES);
        let bar_comp = loader.load().into_diagnostic()?;
    }

    info!("starting foo");

    let loader = monitor_api::CompartmentLoader::new("foo", NewCompartmentFlags::empty());
    let hw_comp = loader.load().into_diagnostic()?;

    info!("starting foo 2");
    let loader = monitor_api::CompartmentLoader::new("foo", NewCompartmentFlags::empty());
    let hw_comp = loader.load().into_diagnostic()?;

    loop {}
    Ok(())
}
