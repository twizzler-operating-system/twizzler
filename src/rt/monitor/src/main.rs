#![feature(naked_functions)]
#![feature(thread_local)]
#![feature(new_zeroed_alloc)]
#![feature(iterator_try_collect)]
#![feature(linkage)]

use std::alloc::GlobalAlloc;

use dynlink::context::NewCompartmentFlags;
use miette::IntoDiagnostic;
use monitor_api::{CompartmentFlags, CompartmentHandle, CompartmentLoader};
use tracing::{debug, info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use twizzler_abi::{object::NULLPAGE_SIZE, simple_mutex::Mutex};
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
        .without_time()
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
                    .args(&["montest", "--test-threads=1"])
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

    info!("monitor early init completed, starting init",);
    let mut args = vec!["init".to_string()];
    for arg in std::env::args() {
        args.push(arg);
    }
    let comp: CompartmentHandle =
        CompartmentLoader::new("init", "init", NewCompartmentFlags::empty())
            .args(&args)
            .load()
            .into_diagnostic()?;
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = comp.wait(flags);
    }

    tracing::warn!("init exited");

    Ok(())
}

struct MonAlloc {
    track: Mutex<Track>,
}

struct Track {
    ips: [usize; 4096],
    count: [usize; 4096],
    idx: usize,
}

#[allow(dead_code)]
impl Track {
    const fn new() -> Self {
        Self {
            ips: [0; 4096],
            count: [0; 4096],
            idx: 0,
        }
    }

    fn insert(&mut self, ip: *mut u8) {
        let addr = ip.addr() + 1;
        let existing = self.ips.iter().position(|i| *i == addr);
        if let Some(existing) = existing {
            self.count[existing] += 1;
        } else if self.idx < 4096 {
            self.ips[self.idx] = addr;
            self.count[self.idx] = 1;
            self.idx += 1;
        } else {
            twizzler_abi::klog_println!("dropping ip {:x}", addr);
        }
    }

    fn reset(&mut self) {
        self.idx = 0;
        self.ips.fill(0);
        self.count.fill(0);
    }

    fn print(&self) {
        for pair in self.ips[0..self.idx]
            .iter()
            .zip(self.count[0..self.idx].iter())
        {
            twizzler_abi::klog_println!("==> {:x} {}", pair.0, pair.1);
        }
    }
}

use twizzler_rt_abi::alloc::AllocFlags;
unsafe impl GlobalAlloc for MonAlloc {
    #[track_caller]
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        twizzler_rt_abi::alloc::twz_rt_malloc(layout, AllocFlags::empty())
            .unwrap_or(core::ptr::null_mut())
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        twizzler_rt_abi::alloc::twz_rt_dealloc(ptr, layout, AllocFlags::empty());
    }
}

//#[global_allocator]
static MA: MonAlloc = MonAlloc {
    track: Mutex::new(Track::new()),
};

pub fn print_alloc_stats() {
    MA.track.lock().print();
}

pub fn reset_alloc_stats() {
    MA.track.lock().reset();
}
