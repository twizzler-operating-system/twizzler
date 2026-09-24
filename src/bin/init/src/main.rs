use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Instant,
};

use monitor_api::{
    CompartmentFlags, CompartmentHandle, CompartmentLoader, MonitorState, NewCompartmentFlags,
};
use secgate::TwzError;
use tracing::{info, warn};
use twizzler::{error::RawTwzError, object::RawObject};
use twizzler_abi::{
    object::ObjID,
    pager::{CompletionToKernel, CompletionToPager, RequestFromKernel, RequestFromPager},
    syscall::{sys_ctrl, sys_new_handle, NewHandleFlags, SysCtrlCmd, SysCtrlFlags},
};
use twizzler_queue::Queue;

fn initialize_pager() -> (ObjID, ObjID) {
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
    // The handle is still leaked deliberately -- the shutdown path runs through the pager -- but
    // its instance is recorded so the shutdown does not signal it.
    let instance = pager_comp.info().map(|i| i.id).unwrap_or_default();
    std::mem::forget(pager_comp);
    (instance, bootstrap_id)
}

fn initialize_namer(bootstrap: ObjID) -> (CompartmentHandle, ObjID) {
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
    (nmcomp, root_id.ok().expect("failed to start namer"))
}

fn initialize_devmgr() -> ObjID {
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
    let instance = devcomp.info().map(|i| i.id).unwrap_or_default();
    std::mem::forget(devcomp);
    instance
}

fn initialize_cache() -> CompartmentHandle {
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
    comp
}

fn initialize_display() -> CompartmentHandle {
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
    comp
}

fn initialize_network() -> CompartmentHandle {
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
    comp
}

fn initialize_sshd() -> CompartmentHandle {
    info!("starting ssh server");
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/pkg/twizzler/bin/sshd")
        .expect("failed to find object");
    let comp: CompartmentHandle =
        CompartmentLoader::new("sshd", "sshd", id, NewCompartmentFlags::empty())
            .args(&["sshd"])
            // The address QEMU's hostfwd targets; net-srv reserves it for whoever asks. `env`
            // replaces the inherited environment, so pass ours along with it.
            .env(
                std::env::vars()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .chain(std::iter::once("TWZ_NET_ADDR=10.0.2.15".to_string())),
            )
            .load()
            .expect("failed to initialize ssh server");
    let mut flags = comp.info().unwrap().flags;
    while !flags.contains(CompartmentFlags::READY) {
        flags = comp.wait(flags);
    }
    comp
}

/// Map every `.so` directly in `dir` to its ObjID, for dynlink to resolve `DT_NEEDED` entries by
/// name. Not recursive -- the directories worth scanning are known, and walking a package tree to
/// find them would cost far more than naming them.
fn cache_libdir(dir: &Path) {
    use std::os::twizzler::fs::MetadataExt;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.contains(".so") {
            continue;
        }
        let path = dir.join(entry.file_name());
        match path.metadata() {
            Ok(md) => {
                let _ = monitor_api::libname_map(&name, md.st_objid().into())
                    .inspect_err(|e| tracing::warn!("failed to map {}: {}", name, e));
            }
            Err(e) => tracing::warn!("failed to stat library {}: {}", path.display(), e),
        }
    }
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

    // Published before anything is started, so a compartment that comes up early can tell "init
    // is working on it" from "no init at all".
    let _ = monitor_api::monitor_state_or(MonitorState::RUNNING)
        .inspect_err(|e| warn!("failed to publish monitor state: {}", e));

    // The handles for every server started below. Init used to `mem::forget` each one, so the
    // monitor kept a use count nothing would ever release; they are held here for the lifetime of
    // the system instead and dropped on the way down. Devmgr and the pager are deliberately still
    // forgotten: the shutdown path itself runs through the pager.
    let mut comps: Vec<CompartmentHandle> = Vec::new();

    // Everything getty owns, forwarded verbatim: the flags shaping an autostart exit, then the
    // autostart program and its arguments. Forwarding the arguments is what lets a workload be
    // aimed at something smaller than its defaults -- `--autostart="pagepar /sysroot/lib 4 16"`
    // rather than 2048 files.
    let mut getty_args: Vec<String> = Vec::new();
    let mut in_autostart = false;
    let mut start_unittest = false;
    let mut memhog_free: Option<usize> = None;
    for arg in std::env::args().skip(1) {
        // Everything past the program name is that program's, long options included: the
        // autostart string is appended to the command line last precisely so this holds. Ahead of
        // it, kernel-only flags share the line (`--kernel-arg=--diag`) and must not be mistaken
        // for the program to run -- the run would die looking up an object called "--diag".
        if in_autostart {
            getty_args.push(arg);
            continue;
        }
        match arg.as_str() {
            "--tests" | "--bench" | "--benches" => start_unittest = true,
            // Diagnostic classes (`net`, `pager`, `naming`, or `all`), exported so every
            // compartment inherits the choice without a rebuild: gated instruments check
            // `TWZ_DIAG` once at first use. Passed as `--kernel-arg=--diag=net,pager`; the
            // kernel's own bare `--diag` is a different, exact-match flag and is unaffected.
            a if a.starts_with("--diag=") => {
                std::env::set_var("TWZ_DIAG", &a["--diag=".len()..]);
            }
            // Pager worker and nvme interrupt placement: `spread` (default), `none`, `hard` or
            // `soft`. See pager-srv's `placement`.
            a if a.starts_with("--pager-pin=") => {
                std::env::set_var("TWZ_PAGER_PIN", &a["--pager-pin=".len()..]);
            }
            // Pin memory until only this much is free before anything else is started, so the
            // test binaries have to be replaced rather than cached. See `start_memhog`.
            a if a.starts_with("--memhog-free=") => {
                let Ok(mib) = a["--memhog-free=".len()..].parse::<usize>() else {
                    eprintln!("bad argument {}", a);
                    shutdown(126);
                    return;
                };
                memhog_free = Some(mib);
            }
            // Getty runs the autostart program and takes the guest down when it exits, so the
            // flags shaping that exit travel with it; init keeps its own copy for the exits it
            // still owns.
            "--syncexit" => {
                SYNC_ON_EXIT.store(true, Ordering::Relaxed);
                getty_args.push(arg.clone());
            }
            "--statexit" => {
                DUMP_ON_EXIT.store(true, Ordering::Relaxed);
                getty_args.push(arg.clone());
            }
            "--zerostart" => getty_args.push(arg.clone()),
            a if a.starts_with("--") => {}
            _ => {
                in_autostart = true;
                getty_args.push(arg.clone());
            }
        }
    }
    // One line whichever way it is set, before any server loads: a log with no diagnostic output
    // must be attributable to "off" rather than to a broken instrument, and this is the artifact
    // that says which.
    tracing::info!(
        "TWZDIAG classes={}",
        std::env::var("TWZ_DIAG").as_deref().unwrap_or("off")
    );

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
    comps.push(lbcomp);

    let devmgr_id = initialize_devmgr();

    let (pager_id, bootstrap_id) = initialize_pager();

    let (nmcomp, _root_id) = initialize_namer(bootstrap_id);
    comps.push(nmcomp);

    // `/initrd` first: it holds everything shipped in the boot image, so the common case still
    // hits on the first entry. The `/pkg/*/bin` directories are what make a bare command name
    // resolve for the ported toolchain -- `ld.lld` in particular, which rustc spawns by name and
    // which otherwise needs `-Clinker=<absolute path>` on every native compile.
    std::env::set_var(
        "PATH",
        // `*` is expanded at each lookup, not here: by the runtime's own PATH search (exec.rs's
        // `find_id`) and by brush (`sys/twizzler/fs.rs`). It has to be, because `/pkg` gains a
        // directory whenever a package is installed and this runs once, at boot -- the list this
        // used to spell out had drifted from what the sysroot actually ships in both directions.
        //
        // `/pkg/twizzler/bin` is named ahead of the glob, not left to it: that is where `xtask
        // disk` stages the userspace build and, with it, uuhelper's coreutils aliases, and the
        // glob is expanded in name order, which would put `twizzler` behind every package
        // alphabetically before it. `/initrd` stays first so a name shipped in the boot image
        // still wins over the disk copy of the same program.
        "/initrd:/pkg/twizzler/bin:/pkg/*/bin",
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
    // A Rust toolchain keeps its per-target `libstd-<hash>.so` under `lib/rustlib/<triple>/lib`,
    // not in `lib` itself. Leaving that unmapped made dlopen of anything linked against libstd --
    // every proc macro, so every on-target build using one -- fail to resolve its dependency.
    let rustlib = Path::new("rustlib")
        .join(format!("{}-unknown-twizzler", std::env::consts::ARCH))
        .join("lib");
    for dir in dir {
        let libdir = Path::new("/pkg").join(dir.unwrap().file_name()).join("lib");
        cache_libdir(&libdir.join(&rustlib));
        cache_libdir(&libdir);
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

    comps.push(initialize_cache());
    comps.push(initialize_network());
    comps.push(initialize_display());

    std::env::set_var(
        "PS1",
        "\\[\x1b[1;32m\\]root\x1b[1;35m@twizzler\\[\x1b[0m\\] \\[\x1b[1;34m\\][\\w]\\[\x1b[0m\\]# ",
    );

    comps.push(initialize_sshd());

    if let Some(mib) = memhog_free {
        start_memhog(mib);
    }

    if start_unittest {
        std::env::set_var("TWZ_TEST_MODE", "1");
        // Load and wait for tests to complete
        run_tests();
        std::env::remove_var("TWZ_TEST_MODE");
    }

    let end_time = Instant::now();
    tracing::info!(
        "finished init in {}s",
        (end_time - start_time).as_secs_f32()
    );

    // Everything init starts is started, so the system is up as far as anyone else can tell.
    let _ = monitor_api::monitor_state_or(MonitorState::UP)
        .inspect_err(|e| warn!("failed to publish monitor state: {}", e));

    // The console gets a thread of its own, because the main thread does not come back from the
    // state watch below.
    std::thread::Builder::new()
        .name("getty".into())
        .spawn(move || run_getty(getty_args))
        .expect("failed to spawn the getty thread");

    watch_monitor_state(comps, [devmgr_id, pager_id]);
}

/// Watch the global monitor state and take the machine down when someone asks for it.
///
/// `sys_ctrl(Shutdown)` rather than `sys_debug_shutdown`: it drains the background sync queue and
/// flushes the pager's backing store on the way out, which the debug path does not.
fn watch_monitor_state(mut comps: Vec<CompartmentHandle>, forgotten: [ObjID; 2]) -> ! {
    let mut state = monitor_api::monitor_state().unwrap_or(MonitorState::empty());
    loop {
        if state.contains(MonitorState::SHUTDOWN) {
            info!("shutdown requested");
            // Everything init did not start -- the shell, whatever it launched -- goes first, so
            // that what it was writing is on the far side of the sync below rather than still in
            // a mapping when the servers underneath it start going away.
            let getty: Vec<ObjID> = GETTY_ID.lock().unwrap().iter().copied().collect();
            let mut skip = forgotten.to_vec();
            skip.extend_from_slice(&getty);
            stop_other_compartments(&comps, &skip);
            // After everything that was writing to it, so the last of that output still has a
            // pump to leave through.
            let getty: Vec<CompartmentHandle> = getty
                .into_iter()
                .filter_map(|id| CompartmentHandle::lookup_id(id).ok())
                .collect();
            stop_compartments("console", &getty);
            // Ahead of the signals: `should_sync` is a property of a *mapping*, so a compartment
            // that exits takes its async-durable registrations with it and there is nothing left
            // for the kernel's region walk to find. Bounded, because a wedged pager would
            // otherwise hold the machine up indefinitely at the one point it is trying to leave.
            match sys_ctrl(
                SysCtrlCmd::SyncAll,
                Some(std::time::Duration::from_secs(15)),
                SysCtrlFlags::empty(),
                0,
                0,
                0,
            ) {
                Ok(n) => info!("synced {} objects", n),
                Err(e) => warn!("sync-all failed: {}", e),
            }
            stop_compartments("server", &comps);
            // Reverse start order: a server is dropped before the ones it was started on top of.
            while let Some(comp) = comps.pop() {
                drop(comp);
            }
            // After the drops rather than before them: what the exiting compartments left behind
            // only becomes reapable once the last handle to each is gone, and the shutdown's own
            // sync runs better against a system that is not still holding them.
            match sys_ctrl(SysCtrlCmd::ReapAll, None, SysCtrlFlags::empty(), 0, 0, 0) {
                Ok(n) => info!("reaped {} objects and threads", n),
                Err(e) => warn!("reap-all failed: {}", e),
            }
            let _ = sys_ctrl(SysCtrlCmd::Shutdown, None, SysCtrlFlags::empty(), 0, 0, 0);
            warn!("shutdown returned -- the machine is still up");
        }
        match monitor_api::monitor_state_wait(state) {
            Ok(new) => state = new,
            Err(e) => {
                // Nothing to retry against: without the gate there is no state to read either, so
                // pace the loop rather than spinning on a wait that is not working.
                warn!("failed to wait for monitor state: {}", e);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    }
}

/// The running console's compartment, if any, for the shutdown path.
static GETTY_ID: Mutex<Option<ObjID>> = Mutex::new(None);

/// Keep a console running until shutdown. Getty owns the pty and the shell on it; what is kept
/// here is only the decision to start it again.
fn run_getty(args: Vec<String>) {
    loop {
        let ran = start_getty(&args)
            .inspect_err(|e| warn!("failed to start getty: {}", e))
            .is_ok();
        // A console that exits during shutdown is the shutdown, not a crash.
        if monitor_api::monitor_state()
            .unwrap_or(MonitorState::empty())
            .contains(MonitorState::SHUTDOWN)
        {
            return;
        }
        if ran {
            warn!("getty exited -- restarting it");
        } else {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

fn start_getty(args: &[String]) -> Result<(), TwzError> {
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/getty")?;
    let mut all_args = vec!["getty".to_string()];
    all_args.extend_from_slice(args);
    let comp = CompartmentLoader::new("getty", "getty", id, NewCompartmentFlags::empty())
        .args(&all_args)
        .load()?;
    *GETTY_ID.lock().unwrap() = Some(comp.info()?.id);
    let mut flags = comp.info()?.flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = comp.wait(flags);
    }
    *GETTY_ID.lock().unwrap() = None;
    Ok(())
}

/// Signal the compartments init did not start, by the same escalation as [`stop_compartments`].
///
/// The exclusions are init's own servers (`own`, stopped later, after the sync), the compartments
/// in `skip` (devmgr and the pager, whose handles are deliberately leaked and which the shutdown
/// path itself runs through, plus getty, stopped once the others are gone), init, and the monitor.
fn stop_other_compartments(own: &[CompartmentHandle], skip_ids: &[ObjID]) {
    let mut skip: Vec<ObjID> = own
        .iter()
        .filter_map(|c| c.info().ok().map(|i| i.id))
        .collect();
    skip.extend_from_slice(skip_ids);
    skip.push(monitor_api::MONITOR_INSTANCE_ID);
    if let Ok(me) = CompartmentHandle::current().info() {
        skip.push(me.id);
    }

    let ids = match monitor_api::compartment_ids() {
        Ok(ids) => ids,
        Err(e) => {
            warn!("failed to enumerate compartments: {}", e);
            return;
        }
    };
    let others: Vec<CompartmentHandle> = ids
        .into_iter()
        .filter(|id| !skip.contains(id))
        .filter_map(|id| CompartmentHandle::lookup_id(id).ok())
        .collect();
    stop_compartments("other", &others);
}

/// Ask every binary compartment to stop, escalating to SIGKILL for whatever is still there.
///
/// Library compartments are skipped: a signal is delivered to a compartment's *main thread*, and
/// a compartment loaded as a library has none -- the servers run on whatever thread called into
/// them.
fn stop_compartments(what: &str, comps: &[CompartmentHandle]) {
    // Reverse start order, matching the drop below it.
    let bins: Vec<&CompartmentHandle> = comps
        .iter()
        .rev()
        .filter(|c| {
            c.info()
                .is_ok_and(|i| i.flags.contains(CompartmentFlags::IS_BINARY))
        })
        .collect();

    if bins.is_empty() {
        return;
    }
    info!("stopping {} {} compartment(s)", bins.len(), what);
    for (sig, name) in [(libc::SIGTERM, "SIGTERM"), (libc::SIGKILL, "SIGKILL")] {
        // Signal everything still up before waiting on any of it, so the second compartment's
        // second starts when the first's does rather than after it.
        let mut pending = Vec::new();
        for comp in &bins {
            if has_exited(comp) {
                continue;
            }
            match comp.signal(sig as u64) {
                Ok(()) => pending.push(*comp),
                Err(e) => warn!("failed to send {}: {}", name, e),
            }
        }
        for comp in pending {
            if !wait_for_exit(comp, std::time::Duration::from_secs(1)) {
                warn!("compartment still up 1s after {}", name);
            }
        }
    }
}

/// Whether `comp` has exited, counting "the monitor no longer knows about it" as exited.
fn has_exited(comp: &CompartmentHandle) -> bool {
    comp.info()
        .map_or(true, |i| i.flags.contains(CompartmentFlags::EXITED))
}

/// Poll `comp` for up to `timeout`, reporting whether it exited in time.
///
/// Polled rather than waited on: `CompartmentHandle::wait` has no deadline, and a compartment
/// that ignores the signal would park shutdown forever.
fn wait_for_exit(comp: &CompartmentHandle, timeout: std::time::Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if has_exited(comp) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Set by `--syncexit`. Off by default, because `shutdown` is on the path of every harness run
/// and making them all pay for writeback would move numbers nobody asked to move.
static SYNC_ON_EXIT: AtomicBool = AtomicBool::new(false);

/// Set by `--statexit`: run `SysCtrlCmd::DebugDump` just before shutting down, so that a run whose
/// program has exited still prints the kernel's lock-free counters (the spinlock census among
/// them). Non-verbose deliberately -- the verbose dump adds one block per object, thousands on a
/// build, and would bury what this is for.
static DUMP_ON_EXIT: AtomicBool = AtomicBool::new(false);

/// Take the guest down from a failure that ends the boot before the console is up (a
/// `--memhog-free` run that could not get its hog). The autostart exit is getty's, which keeps
/// the same flags and does the same things with them.
///
/// `sys_debug_shutdown` does *not* drain the background sync queue or flush the pager's backing
/// store, unlike the `sys_ctrl(Shutdown)` that `watch_monitor_state` uses.
fn shutdown(code: u32) {
    if SYNC_ON_EXIT.load(Ordering::Relaxed) {
        let start = Instant::now();
        let r = sys_ctrl(
            SysCtrlCmd::SyncAll,
            Some(std::time::Duration::from_secs(120)),
            SysCtrlFlags::empty(),
            0,
            0,
            0,
        );
        println!("SYNCEXIT ms={} result={:?}", start.elapsed().as_millis(), r);
    }
    if DUMP_ON_EXIT.load(Ordering::Relaxed) {
        twizzler_abi::syscall::sys_debug_perfmark(false);
        let _ = sys_ctrl(SysCtrlCmd::DebugDump, None, SysCtrlFlags::empty(), 0, 0, 0);
    }
    #[allow(deprecated)]
    twizzler_abi::syscall::sys_debug_shutdown(code);
}

/// Start the resident memory hog (`--memhog-free=<MiB>`) and wait until it holds its memory, so
/// everything started after it runs under the pressure it creates. It is never stopped; the guest
/// goes away with it. Failing to start it is fatal: a low-memory run without the hog would pass
/// for the wrong reason.
fn start_memhog(free_mib: usize) {
    use std::io::BufRead;
    let path = "/initrd/memhog";
    let mut child = match std::process::Command::new(path)
        .args(["--free", &free_mib.to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("failed to start {}: {}", path, e);
            shutdown(126);
            return;
        }
    };
    let mut held = false;
    let stdout = child.stdout.as_mut().expect("piped stdout");
    for line in std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
    {
        println!("{}", line);
        if line.starts_with("memhog: holding") {
            held = true;
            break;
        }
    }
    if !held {
        eprintln!("{} exited before holding any memory", path);
        shutdown(126);
        return;
    }
    // Keeps the child and its pipe alive for the life of the system rather than waiting on it.
    std::mem::forget(child);
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
