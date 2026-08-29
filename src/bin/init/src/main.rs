use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

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

/// Set once the terminal sends an in-band resize notification, which proves it honors mode 2048
/// and will report every subsequent resize unprompted. Until then we fall back to polling it.
static INBAND_RESIZE: AtomicBool = AtomicBool::new(false);

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
    let mut flags = nmcomp.info().unwrap().flags;
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
    let mut flags = devcomp.info().unwrap().flags;
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
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(
        Default::default(),
        "/pkg/twizzler/lib/libcache_srv.so",
    )
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
    let mut flags = comp.info().unwrap().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = comp.wait(flags);
    }
    tracing::info!("cache manager ready");
    std::mem::forget(comp);
}

fn initialize_display() {
    info!("starting display manager");
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(
        Default::default(),
        "/pkg/twizzler/lib/libdisplay_srv.so",
    )
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
    let mut flags = comp.info().unwrap().flags;
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
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(
        Default::default(),
        "/pkg/twizzler/lib/libnet_srv.so",
    )
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
    let mut flags = comp.info().unwrap().flags;
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

fn initialize_sshd() {
    info!("starting ssh server");
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/pkg/twizzler/bin/sshd")
        .expect("failed to find object");
    let comp: CompartmentHandle =
        CompartmentLoader::new("sshd", "sshd", id, NewCompartmentFlags::empty())
            .args(&["sshd"])
            .load()
            .expect("failed to initialize ssh server");
    let mut flags = comp.info().unwrap().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = comp.wait(flags);
    }
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

    let start_time = Instant::now();

    // The first bare word names the autostart program; everything after it is that program's
    // arguments. Forwarding them is what lets a workload be aimed at something smaller than its
    // defaults -- `--autostart="pagepar /sysroot/lib 4 16"` rather than 2048 files.
    let mut autostart: Option<String> = None;
    let mut autostart_args: Vec<String> = Vec::new();
    let mut start_unittest = false;
    for arg in std::env::args().skip(1) {
        // Everything past the program name is that program's, long options included: the
        // autostart string is appended to the command line last precisely so this holds. Ahead of
        // it, kernel-only flags share the line (`--kernel-arg=--diag`) and must not be mistaken
        // for the program to run -- the run would die looking up an object called "--diag".
        if autostart.is_some() {
            autostart_args.push(arg);
            continue;
        }
        match arg.as_str() {
            "--tests" | "--bench" | "--benches" => start_unittest = true,
            a if a.starts_with("--") => {}
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
    let mut flags = lbcomp.info().unwrap().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = lbcomp.wait(flags);
    }
    std::mem::forget(lbcomp);

    initialize_devmgr();

    let bootstrap_id = initialize_pager();

    let _root_id = initialize_namer(bootstrap_id);

    // `/initrd` first: it holds everything shipped in the boot image, so the common case still
    // hits on the first entry. The `/pkg/*/bin` directories are what make a bare command name
    // resolve for the ported toolchain -- `ld.lld` in particular, which rustc spawns by name and
    // which otherwise needs `-Clinker=<absolute path>` on every native compile.
    std::env::set_var(
        "PATH",
        // `/pkg/twizzler/bin` is where `xtask disk` stages the userspace build and, with it,
        // uuhelper's coreutils aliases -- ext4 symlinks in the image rather than naming-server
        // nodes rebuilt on every boot. `/initrd` stays first so a name shipped in the boot image
        // still wins over the disk copy of the same program.
        "/initrd:/pkg/twizzler/bin:/pkg/rust/bin:/pkg/lld/bin:/pkg/llvm/bin:/pkg/binutils/bin:/pkg/python/bin",
    );
    // Give `HOME` a definition rather than leaving it unset. The runtime reports "/" for
    // `NameRoot::Home` either way, so this changes no behaviour on its own -- it makes the value
    // visible to programs that read the variable directly, and gives `cd` and `~` one thing to
    // follow when it becomes something other than the root.
    std::env::set_var("HOME", "/");
    std::env::set_var("PYTHON_HISTORY", "/data/.python_history");
    std::env::set_var("TERM", "xterm");

    let _ = std::os::twizzler::fs::symlink("/ext/sysroot/pkg", "/pkg")
        .inspect_err(|e| tracing::warn!("failed to softlink /pkg: {}", e));
    let _ = std::os::twizzler::fs::symlink("/ext/sysroot", "/sysroot")
        .inspect_err(|e| tracing::warn!("failed to softlink /sysroot: {}", e));
    let _ = std::os::twizzler::fs::symlink("/ext/sysroot/etc", "/etc")
        .inspect_err(|e| tracing::warn!("failed to softlink /etc: {}", e));

    let dir = std::fs::read_dir("/pkg").unwrap();
    use std::os::twizzler::fs::MetadataExt;
    tracing::info!("caching library directories");
    for dir in dir {
        let dir = dir.unwrap();
        let libpath = Path::new("/pkg").join(dir.file_name()).join("lib");
        if let Ok(libdir) = std::fs::read_dir(&libpath) {
            for lib in libdir {
                let lib = lib.unwrap();
                let lib = libpath.join(lib.file_name());
                if lib
                    .file_name()
                    .is_some_and(|s| s.to_string_lossy().contains(".so"))
                {
                    let md = lib.metadata().expect("failed to get metadata for library");
                    let id = md.st_objid();
                    monitor_api::libname_map(
                        &lib.file_name().unwrap().to_string_lossy(),
                        id.into(),
                    )
                    .unwrap();
                }
            }
        }
    }

    if let Ok(libdir) = std::fs::read_dir("/sysroot/lib") {
        for lib in libdir {
            let lib = lib.unwrap();
            let lib = Path::new("/sysroot/lib").join(lib.file_name());
            if lib
                .file_name()
                .is_some_and(|s| s.to_string_lossy().contains(".so"))
            {
                let md = lib.metadata().unwrap();
                let id = md.st_objid();
                monitor_api::libname_map(&lib.file_name().unwrap().to_string_lossy(), id.into())
                    .unwrap();
            }
        }
    }

    std::fs::create_dir_all("/tmp").unwrap();

    initialize_cache();
    initialize_network();
    initialize_display();
    initialize_sshd();

    if start_unittest {
        std::env::set_var("TWZ_TEST_MODE", "1");
        // Load and wait for tests to complete
        run_tests();
        std::env::remove_var("TWZ_TEST_MODE");
    }

    println!("Hi, welcome to the basic twizzler test console.");

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

    std::thread::spawn(move || {
        // Ask for in-band resize notifications (mode 2048) first: a terminal that honors it
        // reports its size immediately and again on every resize, which is what makes the
        // polling query below unnecessary. Terminals that don't know the mode ignore it, so the
        // explicit size query still covers them.
        twizzler_abi::syscall::sys_kernel_console_write(
            twizzler_abi::syscall::KernelConsoleSource::Console,
            b"\x1b[?2048h\x1b[18t",
            KernelConsoleWriteFlags::empty(),
        );

        let mut ansi_buf = Vec::new();
        let mut intercept_mode = false;

        loop {
            let mut buf = [0; 1024];
            let count = twizzler_abi::syscall::sys_kernel_console_read(
                twizzler_abi::syscall::KernelConsoleSource::Console,
                &mut buf,
                KernelConsoleReadFlags::empty(),
            )
            .unwrap();

            // State machine to intercept \x1b[18t ANSI handshakes for terminal size.
            let mut out_buf = Vec::new();
            for &b in &buf[0..count] {
                if !intercept_mode {
                    if b == b'\x1b' {
                        intercept_mode = true;
                        ansi_buf.clear();
                        ansi_buf.push(b);
                    } else {
                        out_buf.push(b);
                    }
                } else {
                    ansi_buf.push(b);
                    if ansi_buf.len() == 3 {
                        // `\x1b[8;..t` is the reply to our size query; `\x1b[48;..t` is an
                        // in-band resize notification. Everything else is real input and must
                        // not be held up here.
                        if ansi_buf[1] != b'[' || (ansi_buf[2] != b'8' && ansi_buf[2] != b'4') {
                            out_buf.extend_from_slice(&ansi_buf);
                            intercept_mode = false;
                        }
                    } else if ansi_buf.len() == 4 && ansi_buf[2] == b'4' && ansi_buf[3] != b'8' {
                        // A `\x1b[4..` sequence that isn't `48`, such as the End key. Release it
                        // rather than buffering to the 32-byte cutoff.
                        out_buf.extend_from_slice(&ansi_buf);
                        intercept_mode = false;
                    } else if ansi_buf.len() > 3 {
                        if b == b't' {
                            let s = String::from_utf8_lossy(&ansi_buf);
                            let body = s
                                .strip_prefix("\x1b[")
                                .and_then(|body| body.strip_suffix('t'));
                            // `48;rows;cols;height;width` is an unprompted resize notification
                            // and carries pixel dimensions; `8;rows;cols` is the query reply.
                            let (inband, params) = match body {
                                Some(body) => match body.strip_prefix("48;") {
                                    Some(rest) => (true, Some(rest)),
                                    None => (false, body.strip_prefix("8;")),
                                },
                                None => (false, None),
                            };
                            if let Some(params) = params {
                                let parts: Vec<&str> = params.split(';').collect();
                                if parts.len() >= 2 {
                                    if let (Ok(r), Ok(c)) =
                                        (parts[0].parse::<u16>(), parts[1].parse::<u16>())
                                    {
                                        let (ws_ypixel, ws_xpixel) = if parts.len() >= 4 {
                                            (
                                                parts[2].parse::<u16>().unwrap_or(0),
                                                parts[3].parse::<u16>().unwrap_or(0),
                                            )
                                        } else {
                                            (0, 0)
                                        };
                                        let winsize = libc::winsize {
                                            ws_row: r,
                                            ws_col: c,
                                            ws_xpixel,
                                            ws_ypixel,
                                        };
                                        unsafe {
                                            let _ = twizzler_rt_abi::bindings::twz_rt_fd_set_config(
                                                server_fd,
                                                twizzler_rt_abi::bindings::IO_REGISTER_WINSIZE,
                                                &winsize as *const _ as *const core::ffi::c_void,
                                                std::mem::size_of::<libc::winsize>(),
                                            );
                                        }
                                        if inband {
                                            INBAND_RESIZE.store(true, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                            intercept_mode = false;
                        } else if ansi_buf.len() > 32 {
                            out_buf.extend_from_slice(&ansi_buf);
                            intercept_mode = false;
                        }
                    }
                }
            }

            if !out_buf.is_empty() {
                let mut ioc = twizzler_rt_abi::io::IoCtx::default();
                let mut done = 0;
                while done < out_buf.len() {
                    done += twizzler_rt_abi::io::twz_rt_fd_pwrite(
                        server_fd,
                        &out_buf[done..],
                        &mut ioc,
                    )
                    .unwrap();
                }
            }
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

    std::thread::spawn(move || loop {
        // Fallback for terminals that ignored mode 2048 above: poll for the size, since nothing
        // else will tell us it changed. A terminal that does support it has already reported in
        // by now and will keep doing so on its own, so stop querying and leave the line quiet.
        std::thread::sleep(std::time::Duration::from_secs(3));
        if INBAND_RESIZE.load(Ordering::Relaxed) {
            return;
        }
        twizzler_abi::syscall::sys_kernel_console_write(
            twizzler_abi::syscall::KernelConsoleSource::Console,
            b"\x1b[18t",
            KernelConsoleWriteFlags::empty(),
        );
    });

    let end_time = Instant::now();
    tracing::info!(
        "finished init in {}s",
        (end_time - start_time).as_secs_f32()
    );

    if let Some(autostart) = autostart {
        run_autostart(&autostart, &autostart_args);
    }

    loop {
        let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/shell")
            .expect("failed to find shell object");
        let mut shell_comp =
            CompartmentLoader::new("shell", "shell", id, NewCompartmentFlags::empty());
        shell_comp.with_controller(monitor_api::ControllerOption::Object(pty.id()));
        shell_comp.args(["shell"]);
        let shell_comp = shell_comp.load().expect("failed to start shell");

        let mut flags = shell_comp.info().unwrap().flags;
        while !flags.contains(CompartmentFlags::EXITED) {
            flags = shell_comp.wait(flags);
        }

        println!("shell exited -- restarting shell");
    }
}

/// Run the program named by `--autostart` and shut the guest down when it exits.
///
/// Two conveniences, both because getting either wrong wastes a whole boot:
///
/// - **`/initrd/<name>` is tried as a fallback.** Every program lives there, so a bare name is what
///   anyone writes, and `--autostart=pagepar` failing on a missing path is a boot spent finding
///   that out. An absolute path still resolves first, so nothing that worked before changes.
/// - **The guest shuts down afterwards**, rather than falling through to the shell loop. An
///   autostart run is unattended by construction -- it is how the harness drives one program -- and
///   a guest that keeps running produces no exit status, so the run ends at whatever silence or
///   progress budget the harness applies instead of when the work finished.
fn run_autostart(autostart: &str, autostart_args: &[String]) {
    // Two fallbacks, in PATH order: the boot image first, then the on-disk program directory.
    // The second is what finds uuhelper's coreutils aliases, which are ext4 symlinks in the image
    // rather than naming-server nodes init used to make -- so `--autostart="ls /"` still works.
    let fallback = format!("/initrd/{}", autostart);
    let disk_fallback = format!("/pkg/twizzler/bin/{}", autostart);
    let resolved = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), autostart)
        .map(|id| (autostart, id))
        .or_else(|_| {
            twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), &fallback)
                .map(|id| (fallback.as_str(), id))
        })
        .or_else(|_| {
            twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), &disk_fallback)
                .map(|id| (disk_fallback.as_str(), id))
        });
    let Ok((path, id)) = resolved else {
        warn!(
            "failed to find autostart program: tried {}, {} and {}",
            autostart, fallback, disk_fallback
        );
        return;
    };

    println!("autostart: {} {:?}", path, autostart_args);
    let mut args = vec![path.to_string()];
    args.extend(autostart_args.iter().cloned());
    let comp = CompartmentLoader::new(path, path, id, NewCompartmentFlags::empty())
        .args(&args)
        .load();
    let Ok(comp) = comp else {
        warn!("failed to start {}", path);
        return;
    };

    let mut flags = comp.info().unwrap().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = comp.wait(flags);
    }
    let exit_code = comp.info().map(|info| info.exit_code).unwrap_or_else(|e| {
        eprintln!("failed to read autostart exit code: {}", e);
        1
    });
    println!("autostart {} finished with code {}", path, exit_code);

    // Temporary (pagerperf.md): a server's counter ring drains on an interval, but the interval is
    // checked from a gate entry, and once the program exits nothing enters a server again -- so
    // its records for the phase just measured would never be printed. Poke the servers on the far
    // side of the interval so they flush before the guest goes down.
    std::thread::sleep(std::time::Duration::from_millis(2500));
    for _ in 0..4 {
        let _ = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd");
        let _ = std::fs::metadata("/initrd");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Same clamp as run_tests: isa-debug-exit reports (code << 1) | 1 in an 8-bit status, so
    // anything above 127 aliases onto another code.
    #[allow(deprecated)]
    twizzler_abi::syscall::sys_debug_shutdown(if exit_code == 0 {
        0
    } else {
        exit_code.min(127) as u32
    });
}

fn run_tests() {
    let id =
        twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/pkg/twizzler/bin/unittest")
            .expect("failed to find unittest object");
    let comp = CompartmentLoader::new("unittest", "unittest", id, NewCompartmentFlags::empty())
        .args(&["unittest"])
        .load()
        .expect("failed to start unittest");
    let mut flags = comp.info().unwrap().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        println!("waiting for comp state change: {:?}", flags);
        flags = comp.wait(flags);
    }

    let exit_code = comp.info().map(|info| info.exit_code).unwrap_or_else(|e| {
        eprintln!("failed to read unittest exit code: {}", e);
        1
    });
    println!("unittests finished with code {}", exit_code);

    // isa-debug-exit reports the code back to the host as (code << 1) | 1 in an 8-bit exit
    // status, so anything above 127 aliases onto another code -- 128 would be indistinguishable
    // from success. Clamp into the representable range, preserving zero vs. nonzero; the REPORT
    // JSON carries the per-test detail.
    let code = if exit_code == 0 {
        0
    } else {
        exit_code.min(127) as u32
    };

    #[allow(deprecated)]
    twizzler_abi::syscall::sys_debug_shutdown(code);
}
