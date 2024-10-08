#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(c_str_literals)]
#![feature(new_uninit)]
#![feature(hash_extract_if)]
#![feature(iterator_try_collect)]
#![feature(result_option_inspect)]

use std::{borrow::Borrow, mem::ManuallyDrop, time::Duration};

use dynlink::context::NewCompartmentFlags;
use miette::IntoDiagnostic;
use mon::get_monitor;
use monitor_api::{CompartmentFlags, CompartmentHandle, CompartmentLoader};
use tracing::{debug, info, warn, Level};
use tracing_subscriber::{fmt::format::FmtSpan, FmtSubscriber};
use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_runtime_api::{get_runtime, MapFlags};
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
        .with_max_level(Level::DEBUG)
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
    if let Some(ki_name) = dlengine::get_kernel_init_info()
        .names()
        .iter()
        .find(|iname| iname.name() == "monitor_test_info")
    {
        info!("starting monitor tests [{}]", ki_name.name());
        let info = get_runtime()
            .map_object(ki_name.id(), MapFlags::READ)
            .unwrap();
        let test_name_slice =
            unsafe { core::slice::from_raw_parts(info.start.add(NULLPAGE_SIZE), 0x1000) };
        let first_null = test_name_slice
            .iter()
            .position(|x| *x == 0)
            .unwrap_or(0x1000 - 1);
        let test_name = String::from_utf8_lossy(&test_name_slice[0..first_null]);
        info!("monitor test binary: {}", test_name);
        if let Some(ki_name) = dlengine::get_kernel_init_info()
            .names()
            .iter()
            .find(|iname| iname.name() == test_name)
        {
            let comp: CompartmentHandle =
                CompartmentLoader::new("montest", test_name, NewCompartmentFlags::empty())
                    .load()
                    .into_diagnostic()?;
            let mut eb = 0;
            let delay_exp_backoff = |state: &mut u64| {
                let val = *state;
                if val < 1000 {
                    if val == 0 {
                        *state = 1;
                    } else {
                        *state *= 2;
                    }
                }
                tracing::info!("sleep: {}", val);
                std::thread::sleep(Duration::from_millis(val));
            };
            while !comp.info().flags.contains(CompartmentFlags::EXITED) {
                delay_exp_backoff(&mut eb);
            }
        } else {
            tracing::error!("failed to start monitor tests: {}", ki_name.name());
        }
    }

    let test_bin = CompartmentLoader::new("montest", "montest", NewCompartmentFlags::empty());
    let test_bin = test_bin.load().into_diagnostic()?;

    loop {}
    info!("monitor early init completed, starting init");

    info!("starting logboi...");
    let loader = monitor_api::CompartmentLoader::new(
        "logboi",
        "liblogboi_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    );
    let logboi_comp = loader.load().into_diagnostic()?;

    // we want logboi to stick around
    let logboi_comp = ManuallyDrop::new(logboi_comp);

    if true {
        info!("starting global bar");
        let loader = monitor_api::CompartmentLoader::new(
            "bar",
            "libbar_srv.so",
            NewCompartmentFlags::EXPORT_GATES,
        );
        let bar_comp = loader.load().into_diagnostic()?;
        let _ = ManuallyDrop::new(bar_comp);
    }

    let stats = get_monitor().stats();
    info!("stats1: {:#?}", stats);

    info!("starting foo");

    let loader = monitor_api::CompartmentLoader::new("foo", "foo", NewCompartmentFlags::empty());
    let foo = loader.load().into_diagnostic()?;

    loop {
        let info = foo.info();
        if info.flags.contains(CompartmentFlags::EXITED) {
            break;
        }
    }

    drop(foo);
    let stats = get_monitor().stats();
    info!("stats2: {:#?}", stats);

    return Ok(());

    info!("starting foo 2");
    let loader = monitor_api::CompartmentLoader::new("foo2", "foo", NewCompartmentFlags::empty());
    let foo2 = loader.load().into_diagnostic()?;

    loop {}
    Ok(())
}
