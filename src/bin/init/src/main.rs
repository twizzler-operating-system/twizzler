use monitor_api::{CompartmentFlags, CompartmentHandle, CompartmentLoader, NewCompartmentFlags};
use tracing::{info, warn};
use twizzler::{error::RawTwzError, object::RawObject};
use twizzler_abi::{
    object::ObjID,
    pager::{CompletionToKernel, CompletionToPager, RequestFromKernel, RequestFromPager},
    syscall::{
        sys_new_handle, KernelConsoleReadFlags, KernelConsoleWriteFlags, NewHandleFlags,
        ObjectCreate,
    },
};
use twizzler_io::pty::DEFAULT_TERMIOS;
use twizzler_queue::Queue;

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

    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "libpager_srv.so")
        .expect("failed to find object");
    let pager_comp: CompartmentHandle = monitor_api::CompartmentLoader::new(
        "pager-srv",
        "libpager_srv.so",
        id,
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
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "libnaming_srv.so")
        .expect("failed to find object");
    let nmcomp: CompartmentHandle = CompartmentLoader::new(
        "naming",
        "libnaming_srv.so",
        id,
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
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "libdevmgr_srv.so")
        .expect("failed to find object");
    let devcomp: CompartmentHandle = CompartmentLoader::new(
        "devmgr",
        "libdevmgr_srv.so",
        id,
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
    let id =
        twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/libcache_srv.so")
            .expect("failed to find object");
    let comp: CompartmentHandle = CompartmentLoader::new(
        "cache",
        "libcache_srv.so",
        id,
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

fn initialize_display() {
    info!("starting display manager");
    let id =
        twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/libdisplay_srv.so")
            .expect("failed to find object");
    let comp: CompartmentHandle = CompartmentLoader::new(
        "display",
        "libdisplay_srv.so",
        id,
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["display-srv"])
    .load()
    .expect("failed to initialize display manager");
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = comp.wait(flags);
    }
    let start_display = unsafe {
        comp.dynamic_gate::<(), RawTwzError>("start_display")
            .unwrap()
    };
    let _ = start_display();
    tracing::info!("display manager ready");
    std::mem::forget(comp);
}

fn initialize_network() {
    info!("starting network manager");
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/libnet_srv.so")
        .expect("failed to find object");
    let comp: CompartmentHandle = CompartmentLoader::new(
        "net",
        "libnet_srv.so",
        id,
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["net-srv"])
    .load()
    .expect("failed to initialize network manager");
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = comp.wait(flags);
    }
    let start_net = unsafe {
        comp.dynamic_gate::<(), RawTwzError>("start_network")
            .unwrap()
    };
    let _ = start_net();
    std::mem::forget(comp);
}

fn _initialize_sgtest() {
    info!("starting sgtest");
    let id =
        twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/libsgtest_srv.so")
            .expect("failed to find object");
    let comp: CompartmentHandle = CompartmentLoader::new(
        "sgtest",
        "libsgtest_srv.so",
        NewCompartmentFlags::EXPORT_GATES,
    )
    .args(&["display-srv"])
    .load()
    .expect("failed to initialize sgtest");
    let mut flags = comp.info().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = comp.wait(flags);
    }
    tracing::info!("sgtest ready");
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
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "liblogboi_srv.so")
        .expect("failed to find object");
    let lbcomp: CompartmentHandle = CompartmentLoader::new(
        "logboi",
        "liblogboi_srv.so",
        id,
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

    let _root_id = initialize_namer(bootstrap_id);

    initialize_cache();
    initialize_network();
    initialize_display();

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

    std::env::set_var("PATH", "/initrd");

    let pty =
        twizzler_io::pty::PtyBase::create_object(ObjectCreate::default(), DEFAULT_TERMIOS).unwrap();
    twizzler_rt_abi::fd::twz_rt_fd_close(0);
    let client_fd = twizzler_rt_abi::fd::twz_rt_fd_open_pty_client(pty.id().raw(), 0).unwrap();
    assert_eq!(client_fd, 0);
    twizzler_rt_abi::fd::twz_rt_fd_close(1);
    let client_fd = twizzler_rt_abi::fd::twz_rt_fd_open_pty_client(pty.id().raw(), 0).unwrap();
    assert_eq!(client_fd, 1);
    twizzler_rt_abi::fd::twz_rt_fd_close(2);
    let client_fd = twizzler_rt_abi::fd::twz_rt_fd_open_pty_client(pty.id().raw(), 0).unwrap();
    assert_eq!(client_fd, 2);
    let server_fd = twizzler_rt_abi::fd::twz_rt_fd_open_pty_server(pty.id().raw(), 0).unwrap();

    std::thread::spawn(move || loop {
        let mut buf = [0; 1024];
        let count = twizzler_abi::syscall::sys_kernel_console_read(
            twizzler_abi::syscall::KernelConsoleSource::Console,
            &mut buf,
            KernelConsoleReadFlags::empty(),
        )
        .unwrap();
        //tracing::info!("Read {} bytes from console: {:?}", count, &buf[0..count]);
        let mut ioc = twizzler_rt_abi::io::IoCtx::default();
        let mut done = 0;
        while done < count {
            done += twizzler_rt_abi::io::twz_rt_fd_pwrite(server_fd, &buf[done..count], &mut ioc)
                .unwrap();
        }
    });

    std::thread::spawn(move || loop {
        let mut buf = [0; 1024];
        let mut ioc = twizzler_rt_abi::io::IoCtx::default();
        let count = twizzler_rt_abi::io::twz_rt_fd_pread(server_fd, &mut buf, &mut ioc).unwrap();
        //tracing::info!("Read {} bytes from pty: {:?}", count, &buf[0..count]);
        twizzler_abi::syscall::sys_kernel_console_write(
            twizzler_abi::syscall::KernelConsoleSource::Console,
            &buf[0..count],
            KernelConsoleWriteFlags::empty(),
        );
    });

    if let Some(autostart) = autostart {
        let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), &autostart)
            .expect("failed to find autostart object");
        println!("autostart: {}", autostart);
        let comp = CompartmentLoader::new(&autostart, &autostart, id, NewCompartmentFlags::empty())
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

    loop {
        let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/shell")
            .expect("failed to find shell object");
        let mut shell_comp =
            CompartmentLoader::new("shell", "shell", id, NewCompartmentFlags::empty());
        shell_comp.with_controller(monitor_api::ControllerOption::Object(pty.id()));
        shell_comp.args(["shell"]);
        let shell_comp = shell_comp.load().expect("failed to start shell");

        let mut flags = shell_comp.info().flags;
        while !flags.contains(CompartmentFlags::EXITED) {
            flags = shell_comp.wait(flags);
        }

        println!("shell exited -- restarting shell");
    }
}

fn run_tests() {
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/unittest")
        .expect("failed to find unittest object");
    let comp = CompartmentLoader::new("unittest", "unittest", id, NewCompartmentFlags::empty())
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
