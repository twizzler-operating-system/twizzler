#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(hash_extract_if)]
#![feature(new_zeroed_alloc)]
#![feature(iterator_try_collect)]
#![feature(linkage)]

use dynlink::context::NewCompartmentFlags;
use miette::IntoDiagnostic;
use monitor_api::{CompartmentFlags, CompartmentHandle, CompartmentLoader};
use tracing::{debug, info, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_rt_abi::object::MapFlags;

mod dlengine;
pub mod init;
mod mon;
mod upcall;

pub use monitor_api::MappedObjectAddrs;

#[path = "../secapi/gates.rs"]
mod gates;

extern crate dynlink;
extern crate twizzler_runtime;

extern "C-unwind" {
    fn __monitor_ready();
}

pub fn main() {
    // For early init, if something breaks, we really want to see everything...
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

    debug!("monitor entered, discovering dynlink context");
    let init =
        init::bootstrap_dynlink_context().expect("failed to discover initial dynlink context");

    let mon = mon::Monitor::new(init);
    mon::set_monitor(mon);

    debug!("ok, starting monitor proper");
    // Safety: the monitor is ready, and so we can set our runtime as ready to use the monitor.
    unsafe { __monitor_ready() };
    // Had to wait till now to be able to spawn threads.
    mon::get_monitor().start_background_threads();

    std::env::set_var("RUST_BACKTRACE", "1");
    unsafe {
        twizzler_rt_abi::bindings::twz_rt_set_upcall_handler(Some(
            crate::upcall::upcall_monitor_handler_entry,
        ))
    };

    let main_thread = std::thread::spawn(monitor_init);
    let _r = main_thread.join().unwrap().map_err(|e| {
        tracing::error!("{:?}", e);
    });
    warn!("monitor main thread exited");
}

fn monitor_init() -> miette::Result<()> {
    // If we have monitor tests to run, do so.
    if let Some(ki_name) = dlengine::get_kernel_init_info()
        .names()
        .iter()
        .find(|iname| iname.name() == "monitor_test_info")
    {
        info!("starting monitor tests [{}]", ki_name.name());
        // Read the monitor test binary name.
        const MAX_NAMELEN: usize = 0x1000;
        let info =
            twizzler_rt_abi::object::twz_rt_map_object(ki_name.id(), MapFlags::READ).unwrap();
        let test_name_slice =
            unsafe { core::slice::from_raw_parts(info.start().add(NULLPAGE_SIZE), MAX_NAMELEN) };
        let first_null = test_name_slice
            .iter()
            .position(|x| *x == 0)
            .unwrap_or(MAX_NAMELEN - 1);
        let test_name = String::from_utf8_lossy(&test_name_slice[0..first_null]);
        debug!("monitor test binary: {}", test_name);
        if let Some(_ki_name) = dlengine::get_kernel_init_info()
            .names()
            .iter()
            .find(|iname| iname.name() == test_name)
        {
            // Load and wait for tests to complete
            let comp: CompartmentHandle =
                CompartmentLoader::new("montest", test_name, NewCompartmentFlags::empty())
                    .args(&["montest"])
                    .load()
                    .into_diagnostic()?;
            let mut flags = comp.info().flags;
            while !flags.contains(CompartmentFlags::EXITED) {
                flags = comp.wait(flags);
            }
        } else {
            tracing::error!("failed to start monitor tests: {}", ki_name.name());
        }
    }
    info!("monitor early init completed, starting init");

    // Load and wait for tests to complete
    let lbcomp: CompartmentHandle = CompartmentLoader::new(
        "logboi",
        "liblogboi_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["logboi"])
    .load()
    .into_diagnostic()?;
    let mut flags = lbcomp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = lbcomp.wait(flags);
    }
    info!("logboi ready");
    std::mem::forget(lbcomp);

    let lbcomp: CompartmentHandle = CompartmentLoader::new(
        "naming_srv`",
        "libnaming_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["naming"])
    .load()
    .into_diagnostic()?;
    let mut flags = lbcomp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = lbcomp.wait(flags);
    }
    info!("naming ready");
    std::mem::forget(lbcomp);

    info!("running logboi test");
    // Load and wait for tests to complete
    let comp: CompartmentHandle =
        CompartmentLoader::new("logboi-test", "logboi-test", NewCompartmentFlags::empty())
            .args(&["logboi-test"])
            .load()
            .into_diagnostic()?;
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = comp.wait(flags);
    }
    info!("running logboi test again");
    // Load and wait for tests to complete
    let comp: CompartmentHandle =
        CompartmentLoader::new("logboi-test", "logboi-test", NewCompartmentFlags::empty())
            .args(&["logboi-test"])
            .load()
            .into_diagnostic()?;
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = comp.wait(flags);
    }

    let comp: CompartmentHandle =
        CompartmentLoader::new("init", "init", NewCompartmentFlags::empty())
            .args(&["init"])
            .load()
            .into_diagnostic()?;
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = comp.wait(flags);
    }

    tracing::warn!("init exited");

    Ok(())
}
