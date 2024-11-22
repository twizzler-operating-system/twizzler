#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(hash_extract_if)]
#![feature(new_zeroed_alloc)]
#![feature(iterator_try_collect)]

use dynlink::engines::Backing;
use tracing::{debug, info, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_object::ObjID;
use twizzler_rt_abi::object::MapFlags;
use twz_rt::{set_upcall_handler, OUR_RUNTIME};

mod dlengine;
mod init;
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
        .with_max_level(Level::DEBUG)
        .with_target(false)
        .with_span_events(FmtSpan::ACTIVE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    miette::set_hook(Box::new(|_| {
        Box::new(miette::NarratableReportHandler::new().with_cause_chain())
    }))
    .unwrap();

    debug!("monitor entered, discovering dynlink context");
    let init =
        init::bootstrap_dynlink_context().expect("failed to discover initial dynlink context");

    let mon = mon::Monitor::new(init);
    mon::set_monitor(mon);

    // Safety: the monitor is ready, and so we can set our runtime as ready to use the monitor.
    unsafe { OUR_RUNTIME.set_runtime_ready() };
    // Had to wait till now to be able to spawn threads.
    mon::get_monitor().start_background_threads();

    debug!("Ok");
    std::env::set_var("RUST_BACKTRACE", "1");
    set_upcall_handler(&crate::upcall::upcall_monitor_handler).unwrap();

    let main_thread = std::thread::spawn(|| monitor_init());
    let _r = main_thread.join().unwrap().map_err(|e| {
        tracing::error!("{:?}", e);
    });
    warn!("monitor main thread exited");
}

fn monitor_init() -> miette::Result<()> {
    info!("monitor early init completed, starting init");

    Ok(())
}
