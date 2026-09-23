//! The console: owns the pty behind the kernel console and the shell running on it.
//!
//! Loaded by init once the servers are up, and respawned by it if this exits. The pty object is
//! ours, so a restart is a fresh pty and a fresh shell; what init keeps is only the decision to
//! start again. Every byte between the kernel console and any program's stdio passes through the
//! two pump threads here, and the terminal-size handshake with whatever is on the serial line is
//! handled here too. With an autostart program named on the command line, that program runs on
//! the pty instead of the shell and the guest shuts down when it exits.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use monitor_api::{CompartmentFlags, CompartmentLoader, MonitorState, NewCompartmentFlags};
use tracing::warn;
use twizzler_abi::{
    object::ObjID,
    syscall::{
        sys_ctrl, KernelConsoleReadFlags, KernelConsoleWriteFlags, ObjectCreate, SysCtrlCmd,
        SysCtrlFlags,
    },
};
use twizzler_io::pty::DEFAULT_TERMIOS;
use twizzler_rt_abi::error::TwzError;

/// Set once the terminal sends an in-band resize notification, which proves it honors mode 2048
/// and will report every subsequent resize unprompted. Until then we fall back to polling it.
static INBAND_RESIZE: AtomicBool = AtomicBool::new(false);

/// Set by `--syncexit`: run `SysCtrlCmd::SyncAll` before an autostart shutdown and print how long
/// it took, so the writeback an autostart run otherwise discards is visible in its wall time.
static SYNC_ON_EXIT: AtomicBool = AtomicBool::new(false);

/// Set by `--statexit`: run `SysCtrlCmd::DebugDump` just before shutting down, so that a run whose
/// program has exited still prints the kernel's lock-free counters (the spinlock census among
/// them). Non-verbose deliberately -- the verbose dump adds one block per object, thousands on a
/// build, and would bury what this is for.
static DUMP_ON_EXIT: AtomicBool = AtomicBool::new(false);

/// Set by `--zerostart`: sweep every free physical frame to zero *before* the autostart program
/// runs, so that page-fault-time zeroing is not on the workload's critical path.
///
/// The control for "is the guest paying to zero pages while it works": with every free frame
/// already clean the fault path installs them directly, so a workload that speeds up here was
/// bounded by zeroing and one that does not was not. `zero_all` is bounded by its own pass cap
/// and by the timeout passed here.
static ZERO_ON_START: AtomicBool = AtomicBool::new(false);

fn main() {
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .without_time()
            .finish(),
    )
    .unwrap();

    // The first bare word names the autostart program; everything after it is that program's
    // arguments, long options included -- init appends the autostart string to our command line
    // last precisely so this holds.
    let mut autostart: Option<String> = None;
    let mut autostart_args: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        if autostart.is_some() {
            autostart_args.push(arg);
            continue;
        }
        match arg.as_str() {
            "--syncexit" => SYNC_ON_EXIT.store(true, Ordering::Relaxed),
            "--statexit" => DUMP_ON_EXIT.store(true, Ordering::Relaxed),
            "--zerostart" => ZERO_ON_START.store(true, Ordering::Relaxed),
            a if a.starts_with("--") => {}
            _ => autostart = Some(arg),
        }
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

    let _ = std::thread::Builder::new()
        .name("pty-input".into())
        .spawn(move || {
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

                // Pull terminal size reports (`\x1b[8;rows;colst` replies and `\x1b[48;...t`
                // in-band resize notifications) out of the input stream. Everything
                // else, including replies to other queries programs make
                // (`\x1b[row;colR` for a cursor position), is real input and is
                // passed through as soon as the sequence is known not to be ours: a CSI
                // ends at its first final byte (0x40..=0x7E), so holding past that starved the
                // program of any reply whose parameters merely started with 8 or 48.
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
                        continue;
                    }
                    if b == b'\x1b' {
                        // A new escape restarts the sequence; whatever was held was not a report.
                        out_buf.extend_from_slice(&ansi_buf);
                        ansi_buf.clear();
                        ansi_buf.push(b);
                        continue;
                    }
                    ansi_buf.push(b);
                    if ansi_buf.len() == 2 {
                        if b != b'[' {
                            out_buf.extend_from_slice(&ansi_buf);
                            intercept_mode = false;
                        }
                        continue;
                    }
                    let is_final = (0x40..=0x7e).contains(&b);
                    if !is_final {
                        if ansi_buf.len() > 32 {
                            out_buf.extend_from_slice(&ansi_buf);
                            intercept_mode = false;
                        }
                        continue;
                    }
                    intercept_mode = false;
                    let consumed = b == b't' && report_winsize(&ansi_buf, server_fd);
                    if !consumed {
                        out_buf.extend_from_slice(&ansi_buf);
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
        })
        .unwrap();

    let _ = std::thread::Builder::new()
        .name("pty-console".into())
        .spawn(move || loop {
            let mut buf = [0; 1024];
            let mut ioc = twizzler_rt_abi::io::IoCtx::default();
            // Never unwrap: this thread is the *only* path from any program's stdout to the
            // console, and it has no supervisor. The unwrap this replaces turned one transient
            // read error into permanent, silent loss of all console output for the rest of the
            // boot -- which reads as a hung or mute system rather than as a failed read.
            let count = match twizzler_rt_abi::io::twz_rt_fd_pread(server_fd, &mut buf, &mut ioc) {
                Ok(count) => count,
                Err(_) => continue,
            };
            if count == 0 {
                continue;
            }
            twizzler_abi::syscall::sys_kernel_console_write(
                twizzler_abi::syscall::KernelConsoleSource::Console,
                &buf[0..count],
                KernelConsoleWriteFlags::empty(),
            );
        })
        .unwrap();

    let _ = std::thread::Builder::new()
        .name("pty-resize".into())
        .spawn(move || loop {
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
        })
        .unwrap();

    let pty_id = pty.id();
    if let Some(autostart) = autostart {
        run_autostart(&autostart, &autostart_args);
        return;
    }
    loop {
        if run_brush(pty_id).is_err() {
            warn!("failed to start brush");
            run_shell(pty_id).expect("failed to start any shell");
        }
        // A shell that exits during shutdown is the shutdown, not a crash: stop respawning and
        // stay quiet, since the console is going away anyway.
        if monitor_api::monitor_state()
            .unwrap_or(MonitorState::empty())
            .contains(MonitorState::SHUTDOWN)
        {
            return;
        }
        println!("shell exited -- restarting shell");
    }
}

fn run_shell(pty_id: ObjID) -> Result<(), TwzError> {
    let id = twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/initrd/shell")?;
    let mut shell_comp = CompartmentLoader::new("shell", "shell", id, NewCompartmentFlags::empty());
    shell_comp.with_controller(monitor_api::ControllerOption::Object(pty_id));
    shell_comp.args(["shell"]);
    let shell_comp = shell_comp.load()?;

    let mut flags = shell_comp.info().unwrap().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = shell_comp.wait(flags);
    }

    Ok(())
}

fn run_brush(pty_id: ObjID) -> Result<(), TwzError> {
    let id =
        twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), "/pkg/twizzler/bin/brush")?;
    let mut shell_comp = CompartmentLoader::new("brush", "brush", id, NewCompartmentFlags::empty());
    shell_comp.with_controller(monitor_api::ControllerOption::Object(pty_id));
    shell_comp.args(["brush"]);
    let shell_comp = shell_comp.load()?;

    let mut flags = shell_comp.info().unwrap().flags;
    while !flags.contains(CompartmentFlags::EXITED) {
        flags = shell_comp.wait(flags);
    }
    Ok(())
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
///
/// That second point holds for the failure paths as well, and it did not used to. Warning and
/// returning left the shell loop to run, so a name that resolved to nothing kept the guest alive
/// until the harness gave up: an observed mistyped `--autostart` cost 5m22s and was reported as
/// "no test report (timeout or early exit)", which reads like a hang rather than a typo. The two
/// failures exit with the shell's codes for them -- 127 for a name that resolved to nothing, 126
/// for one that resolved but could not be run -- so the harness's status distinguishes them from
/// anything the program itself returns.
fn run_autostart(autostart: &str, autostart_args: &[String]) {
    // With `--statexit`, rebaseline the kernel's profiles here so the counters printed at exit
    // cover the autostart program alone rather than boot plus the program.
    if DUMP_ON_EXIT.load(Ordering::Relaxed) {
        twizzler_abi::syscall::sys_debug_perfmark(true);
    }
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
        shutdown(127);
        return;
    };

    if ZERO_ON_START.load(Ordering::Relaxed) {
        let start = Instant::now();
        let r = sys_ctrl(
            SysCtrlCmd::ZeroAll,
            Some(std::time::Duration::from_secs(120)),
            SysCtrlFlags::empty(),
            0,
            0,
            0,
        );
        println!("ZEROSTART ms={} bytes={:?}", start.elapsed().as_millis(), r);
    }

    println!("autostart: {} {:?}", path, autostart_args);
    let mut args = vec![path.to_string()];
    args.extend(autostart_args.iter().cloned());
    let comp = CompartmentLoader::new(path, path, id, NewCompartmentFlags::empty())
        .args(&args)
        .load();
    let Ok(comp) = comp else {
        warn!("failed to start {}", path);
        shutdown(126);
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

    // Same clamp as init's run_tests: isa-debug-exit reports (code << 1) | 1 in an 8-bit status,
    // so anything above 127 aliases onto another code. Clamped before the cast, since `exit_code`
    // is wider than what is being clamped to.
    shutdown(if exit_code == 0 {
        0
    } else {
        exit_code.min(127) as u32
    });
}

/// Take the guest down. Every exit from `run_autostart` goes through here, including the two
/// failures -- the whole point of the function is that the boot ends when the program does, and a
/// path that returns instead hands the harness a timeout to interpret.
///
/// `sys_debug_shutdown` does *not* drain the background sync queue or flush the pager's backing
/// store, unlike the `sys_ctrl(Shutdown)` that init's state watch uses. So an `--autostart` run
/// discards whatever is still dirty when its program exits, and the wall time it reports excludes
/// that writeback entirely. `--syncexit` makes the cost visible: it runs the same `SyncAll` first
/// and prints how long it took.
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

/// Apply a `\x1b[8;rows;colst` size reply or `\x1b[48;rows;cols;height;widtht` in-band resize
/// notification to the pty. Returns whether `seq` was one of those; anything else is untouched.
fn report_winsize(seq: &[u8], server_fd: twizzler_rt_abi::fd::RawFd) -> bool {
    let s = String::from_utf8_lossy(seq);
    let Some(body) = s
        .strip_prefix("\x1b[")
        .and_then(|body| body.strip_suffix('t'))
    else {
        return false;
    };
    let (inband, params) = match body.strip_prefix("48;") {
        Some(rest) => (true, rest),
        None => match body.strip_prefix("8;") {
            Some(rest) => (false, rest),
            None => return false,
        },
    };
    let parts: Vec<&str> = params.split(';').collect();
    if parts.len() < 2 {
        return false;
    }
    let (Ok(r), Ok(c)) = (parts[0].parse::<u16>(), parts[1].parse::<u16>()) else {
        return false;
    };
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
    true
}
