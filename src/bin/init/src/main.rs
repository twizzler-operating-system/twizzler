use std::io::{Read, Write};

use embedded_io::ErrorType;
use monitor_api::{CompartmentFlags, CompartmentHandle, CompartmentLoader, NewCompartmentFlags};
use tracing::{info, warn};
use twizzler::object::RawObject;
use twizzler_abi::{
    object::ObjID,
    pager::{CompletionToKernel, CompletionToPager, RequestFromKernel, RequestFromPager},
    syscall::{sys_new_handle, NewHandleFlags},
};
use twizzler_queue::Queue;

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
    let queue_obj = unsafe {
        twizzler::object::ObjectBuilder::<()>::default()
            .build_ctor(|obj| {
                twizzler_queue::Queue::<RequestFromKernel, CompletionToKernel>::init(
                    obj.handle(),
                    DEFAULT_PAGER_QUEUE_LEN,
                    DEFAULT_PAGER_QUEUE_LEN,
                )
            })
            .expect("failed to create pager queue")
    };
    let queue = Queue::<RequestFromKernel, CompletionToKernel>::from(queue_obj.into_handle());

    sys_new_handle(
        queue.handle().id(),
        twizzler_abi::syscall::HandleType::PagerQueue,
        NewHandleFlags::empty(),
    )
    .expect("failed to setup pager queue");

    let queue2_obj = unsafe {
        twizzler::object::ObjectBuilder::<()>::default()
            .build_ctor(|obj| {
                twizzler_queue::Queue::<RequestFromPager, CompletionToPager>::init(
                    obj.handle(),
                    DEFAULT_PAGER_QUEUE_LEN,
                    DEFAULT_PAGER_QUEUE_LEN,
                )
            })
            .expect("failed to create pager queue")
    };
    let queue2 = Queue::<RequestFromPager, CompletionToPager>::from(queue2_obj.into_handle());
    sys_new_handle(
        queue2.handle().id(),
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
    let bootstrap_id = pager_start(queue.handle().id(), queue2.handle().id()).unwrap();
    std::mem::forget(pager_comp);
    bootstrap_id
}

fn initialize_namer(bootstrap: ObjID) -> ObjID {
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

    let namer_start = unsafe {
        nmcomp
            .dynamic_gate::<(ObjID,), ObjID>("namer_start")
            .unwrap()
    };
    let root_id = namer_start(bootstrap);
    tracing::info!("naming ready");
    std::mem::forget(nmcomp);
    root_id.ok().expect("failed to start namer")
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
    devmgr_start().unwrap();
    tracing::info!("device manager ready");
    std::mem::forget(devcomp);
}

fn initialize_cache() {
    info!("starting cache service");
    let comp: CompartmentHandle = CompartmentLoader::new(
        "cache",
        "libcache_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["cache-srv"])
    .load()
    .expect("failed to initialize cache manager");
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = comp.wait(flags);
    }
    tracing::info!("cache manager ready");
    std::mem::forget(comp);
}

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();

    let mut autostart = None;
    let mut start_unittest = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--tests" | "--bench" | "--benches" => start_unittest = true,
            _ => autostart = Some(arg),
        }
    }

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

    let root_id = initialize_namer(bootstrap_id);

    // Set new nameroot for the monitor
    tracing::info!("setting monitor nameroot: {}", root_id);
    let _ = monitor_api::set_nameroot(root_id)
        .inspect_err(|_| tracing::warn!("failed to set nameroot for monitor"));

    initialize_cache();

    if start_unittest {
        // Load and wait for tests to complete
        run_tests();
    }

    let utils = [
        "ls", "cat", "base64", "base32", "basename", "basenc", "cksum", "comm", "csplit", "cut",
        "date", "echo", "expand", "factor", "false", "fmt", "fold", "ln", "nl", "numfmt", "od",
        "paste", "pr", "printenv", "printf", "ptx", "seq", "shuf", "sleep", "sort", "sum", "tr",
        "true", "tsort", "unexpand", "uniq", "yes",
    ];
    for util in utils {
        let link = format!("/initrd/{}", util);
        tracing::debug!("creating link: {}", link);
        let _ = std::os::twizzler::fs::symlink("uuhelper", link)
            .inspect_err(|e| tracing::warn!("failed to softlink util {}: {}", util, e));
    }

    println!("Hi, welcome to the basic twizzler test console.");

    if let Some(autostart) = autostart {
        println!("autostart: {}", autostart);
        let comp = CompartmentLoader::new(&autostart, &autostart, NewCompartmentFlags::empty())
            .args(&[&autostart])
            .load();
        if let Ok(comp) = comp {
            let mut flags = comp.info().flags;
            while !flags.contains(CompartmentFlags::EXITED) {
                flags = comp.wait(flags);
            }
        } else {
            warn!("failed to start {}", autostart);
        }
    }

    println!("To run a program, type its name.");

    let mut io = TwzIo;
    let mut buffer = [0; 1024];
    let mut history = [0; 1024];
    let mut editor = noline::builder::EditorBuilder::from_slice(&mut buffer)
        .with_slice_history(&mut history)
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

        let background = cmd.iter().any(|s| *s == "&");

        // Find env vars
        let cmd = cmd.into_iter().map(|s| as_env(s)).collect::<Vec<_>>();
        let vars = cmd
            .iter()
            .filter_map(|r| match r {
                Ok((k, v)) => Some((k, v)),
                Err(_) => None,
            })
            .collect::<Vec<_>>();
        let cmd = cmd
            .iter()
            .filter_map(|r| match r {
                Ok(_) => None,
                Err(s) => Some(s),
            })
            .collect::<Vec<_>>();

        tracing::debug!("got env: {:?}, cmd: {:?}", vars, cmd);

        let comp = CompartmentLoader::new(cmd[0], cmd[0], NewCompartmentFlags::empty())
            .args(&cmd)
            .env(vars.into_iter().map(|(k, v)| format!("{}={}", k, v)))
            .load();
        if let Ok(comp) = comp {
            if background {
                tracing::info!("continuing compartment {} in background", cmd[0]);
            } else {
                let mut flags = comp.info().flags;
                while !flags.contains(CompartmentFlags::EXITED) {
                    flags = comp.wait(flags);
                }
            }
        } else {
            warn!("failed to start {}", cmd[0]);
        }
    }
}

fn as_env<'a>(s: &'a str) -> Result<(&'a str, &'a str), &'a str> {
    let mut split = s.split("=");
    Ok((split.next().ok_or(s)?, split.next().ok_or(s)?))
}

/*
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
*/

fn run_tests() {
    let comp = CompartmentLoader::new("unittest", "unittest", NewCompartmentFlags::empty())
        .args(&["unittest"])
        .load()
        .expect("failed to start unittest");
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        println!("waiting for comp state change: {:?}", flags);
        flags = comp.wait(flags);
    }

    println!("unittests finished");

    #[allow(deprecated)]
    twizzler_abi::syscall::sys_debug_shutdown(0);
}
