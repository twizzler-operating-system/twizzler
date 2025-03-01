use std::io::{Read, Write};

use embedded_io::ErrorType;
use monitor_api::{CompartmentFlags, CompartmentHandle, CompartmentLoader, NewCompartmentFlags};
use tracing::{info, warn};
use twizzler_abi::{
    aux::KernelInitInfo,
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    pager::{CompletionToKernel, RequestFromKernel},
    syscall::{sys_new_handle, BackingType, LifetimeType, NewHandleFlags},
};
use twizzler_object::CreateSpec;

struct TwzIo;

impl ErrorType for TwzIo {
    type Error = std::io::Error;
}

impl embedded_io::Read for TwzIo {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let len = std::io::stdin().read(buf)?;

        Ok(len)
    }
}

impl embedded_io::Write for TwzIo {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        std::io::stdout().write(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        std::io::stdout().flush()
    }
}

fn initialize_pager() -> ObjID {
    info!("starting pager");
    const DEFAULT_PAGER_QUEUE_LEN: usize = 1024;
    let queue = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        DEFAULT_PAGER_QUEUE_LEN,
        DEFAULT_PAGER_QUEUE_LEN,
    )
    .unwrap();

    sys_new_handle(
        queue.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();
    let queue2 = twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::create(
        &CreateSpec::new(LifetimeType::Volatile, BackingType::Normal),
        DEFAULT_PAGER_QUEUE_LEN,
        DEFAULT_PAGER_QUEUE_LEN,
    )
    .unwrap();
    sys_new_handle(
        queue2.object().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .unwrap();

    let pager_comp: CompartmentHandle = monitor_api::CompartmentLoader::new(
        "pager-srv",
        "libpager_srv.so",
        monitor_api::NewCompartmentFlags::EXPORT_GATES,
    )
    .args(["pager-srv"])
    .load()
    .expect("failed to start pager");

    let pager_start = unsafe {
        pager_comp
            .dynamic_gate::<(ObjID, ObjID), ObjID>("pager_start")
            .unwrap()
    };
    let bootstrap_id = pager_start(queue.object().id(), queue2.object().id()).unwrap();
    std::mem::forget(pager_comp);
    bootstrap_id
}

fn initialize_namer(bootstrap: ObjID) {
    info!("starting namer");
    let nmcomp: CompartmentHandle = CompartmentLoader::new(
        "naming",
        "libnaming_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["naming"])
    .load()
    .expect("failed to initialize namer");
    let mut flags = nmcomp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = nmcomp.wait(flags);
    }

    let namer_start = unsafe { nmcomp.dynamic_gate::<(ObjID,), ()>("namer_start").unwrap() };
    namer_start(bootstrap);
    tracing::info!("naming ready");
    std::mem::forget(nmcomp);
}

fn initialize_devmgr() {
    info!("starting device manager");
    let devcomp: CompartmentHandle = CompartmentLoader::new(
        "devmgr",
        "libdevmgr_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["devmgr"])
    .load()
    .expect("failed to initialize device manager");
    let mut flags = devcomp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = devcomp.wait(flags);
    }

    let devmgr_start = unsafe { devcomp.dynamic_gate::<(), ()>("devmgr_start").unwrap() };
    devmgr_start();
    tracing::info!("device manager ready");
    std::mem::forget(devcomp);
}
fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();

    tracing::info!("starting logger");
    let lbcomp: CompartmentHandle = CompartmentLoader::new(
        "logboi",
        "liblogboi_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["logboi"])
    .load()
    .unwrap();
    let mut flags = lbcomp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = lbcomp.wait(flags);
    }
    std::mem::forget(lbcomp);

    initialize_devmgr();

    let bootstrap_id = initialize_pager();

    initialize_namer(bootstrap_id);

    // Load and wait for tests to complete
    run_tests("test_bins", false);
    run_tests("bench_bins", true);

    println!("Hi, welcome to the basic twizzler test console.");
    println!("To run a program, type its name.");

    let mut io = TwzIo;
    let mut buffer = [0; 1024];
    let mut editor = noline::builder::EditorBuilder::from_slice(&mut buffer)
        .build_sync(&mut io)
        .unwrap();
    loop {
        //let mstats = monitor_api::stats().unwrap();
        //println!("{:?}", mstats);
        let line = editor.readline("twz> ", &mut io).unwrap();
        let cmd = line.split_whitespace().collect::<Vec<_>>();
        if cmd.len() == 0 {
            continue;
        }
        let comp = CompartmentLoader::new(cmd[0], cmd[0], NewCompartmentFlags::empty())
            .args(&cmd)
            .load();
        if let Ok(comp) = comp {
            let mut flags = comp.info().flags;
            while !flags.contains(CompartmentFlags::EXITED) {
                flags = comp.wait(flags);
            }
        } else {
            warn!("failed to start {}", cmd[0]);
        }
    }
}

fn get_kernel_init_info() -> &'static KernelInitInfo {
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

fn run_tests(test_list_name: &str, benches: bool) {
    if let Some(id) = find_init_name(test_list_name) {
        println!("=== found init test list ===");
        let handle =
            twizzler_rt_abi::object::twz_rt_map_object(id, Protections::READ.into()).unwrap();

        let addr = unsafe { handle.start().add(NULLPAGE_SIZE) };
        let bytes = unsafe {
            core::slice::from_raw_parts(addr as *const u8, twizzler_abi::object::MAX_SIZE)
        };
        let bytes = &bytes[0..bytes.iter().position(|r| *r == 0).unwrap_or(0)];
        let str = String::from_utf8(bytes.to_vec()).unwrap();
        let test_failed = false;
        for line in str.split("\n").filter(|l| !l.is_empty()) {
            println!("STARTING TEST {}", line);
            let test_comp = monitor_api::CompartmentLoader::new(
                line,
                line,
                monitor_api::NewCompartmentFlags::empty(),
            )
            .args(&[line, if benches { "--bench" } else { "--test" }])
            .load()
            .expect("failed to load specified test");
            let mut flags = test_comp.info().flags;
            while !flags.contains(monitor_api::CompartmentFlags::EXITED) {
                flags = test_comp.wait(flags);
            }
        }
        // TODO: get exit status, and set this
        if test_failed {
            println!("!!! TEST MODE FAILED");
        }
        #[allow(deprecated)]
        twizzler_abi::syscall::sys_debug_shutdown(if test_failed { 1 } else { 0 });
    }
}
