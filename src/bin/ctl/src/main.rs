//! `ctl` -- issue the kernel's system-control commands ([`sys_ctrl`]).
//!
//! All three run inside the kernel on this process's thread, at this thread's priority. That is
//! the point of them: each does work the kernel also does from a `BACKGROUND` thread, and a
//! `BACKGROUND` thread is exactly what a machine busy enough to need the work will not schedule.

use std::{process::ExitCode, time::Duration};

use clap::{Parser, Subcommand};
use monitor_api::MonitorState;
use twizzler_abi::syscall::{KernelDiagFlags, SysCtrlCmd, SysCtrlFlags, sys_ctrl};
use twizzler_rt_abi::error::TwzError;

#[derive(Parser)]
#[command(
    name = "ctl",
    about = "Issue kernel system-control commands.",
    // Any unambiguous prefix of a subcommand name or alias works, so `ctl sy` is `ctl sync-all`.
    infer_subcommands = true
)]
struct Args {
    /// Give up after this many milliseconds, reporting how far it got. Ignored by debug-dump.
    #[arg(long, global = true, value_name = "MS")]
    timeout: Option<u64>,
    /// Submit the work without waiting for it to finish: sync-all does not wait for the pager to
    /// acknowledge, and zero-all makes a single pass instead of sweeping to completion.
    #[arg(long, global = true)]
    no_wait: bool,
    /// Print more. debug-dump only, where it adds the kernel's profile counters and a block per
    /// object -- thousands of lines on a busy system.
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    cmd: Command,
}

/// Each command carries the short names it is worth typing: the `-all`/`-dump` suffixes say what
/// the kernel does, which is right in the help and tedious at a prompt.
#[derive(Subcommand, Clone)]
enum Command {
    /// Print kernel debugging state to the kernel console.
    #[command(visible_aliases = ["dump", "d"])]
    DebugDump,
    /// Sync every mapping registered async-durable and the syncs queued for the kernel's
    /// background sync thread, then flush the pager's backing store.
    #[command(visible_aliases = ["sync", "s"])]
    SyncAll,
    /// Zero every free physical frame.
    #[command(visible_aliases = ["zero", "z"])]
    ZeroAll,
    /// Turn kernel diagnostic classes on and off. With no arguments, print the current set.
    #[command(visible_aliases = ["diag", "g"])]
    SetDiag {
        /// Classes to switch on. Comma-separated; see `--help` for the list.
        #[arg(long, value_delimiter = ',', value_name = "CLASS")]
        on: Vec<DiagClass>,
        /// Classes to switch off. Applied after --on, so naming a class in both leaves it off.
        #[arg(long, value_delimiter = ',', value_name = "CLASS")]
        off: Vec<DiagClass>,
    },
    /// Reap deleted objects and exited threads now, rather than waiting for the idle loop and the
    /// background reaper.
    #[command(visible_aliases = ["reap", "r"])]
    ReapAll,
    /// Ask the system to shut down: set the monitor's SHUTDOWN flag and let init take the
    /// machine down in its own time.
    #[command(visible_alias = "poweroff")]
    Shutdown {
        /// Exit code to hand the host. --immediate only: the monitor state carries flags, not a
        /// code, so a requested shutdown always reports 0.
        #[arg(default_value_t = 0)]
        code: u32,
        /// Skip the monitor and call the kernel directly. Nothing gets a chance to shut down
        /// cleanly first, but it works when init is not there to act on the flag.
        #[arg(long)]
        immediate: bool,
    },
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum DiagClass {
    /// Idle-loop hang diagnostics: stuck-thread, orphan-thread and mutex-timeout scans.
    Diag,
    /// Kernel-side pager and large-page milestone reports.
    Pager,
    /// Per-object invalidation-latch reports.
    Invls,
    /// Wake and scheduling latency reports.
    Wake,
    /// Per-object page-fault census.
    Fault,
    /// Every class above.
    All,
}

impl From<DiagClass> for KernelDiagFlags {
    fn from(c: DiagClass) -> Self {
        match c {
            DiagClass::Diag => KernelDiagFlags::DIAG,
            DiagClass::Pager => KernelDiagFlags::PAGER,
            DiagClass::Invls => KernelDiagFlags::INVLS,
            DiagClass::Wake => KernelDiagFlags::WAKE,
            DiagClass::Fault => KernelDiagFlags::FAULT,
            DiagClass::All => KernelDiagFlags::all(),
        }
    }
}

fn mask(classes: &[DiagClass]) -> u64 {
    classes
        .iter()
        .fold(KernelDiagFlags::empty(), |acc, c| acc | (*c).into())
        .bits()
}

/// The class names behind a returned mask, or `(none)`. Named rather than hex because the point of
/// reading the mask back is to see what is armed.
fn describe(bits: u64) -> String {
    let flags = KernelDiagFlags::from_bits_truncate(bits);
    let names: Vec<&str> = [
        (KernelDiagFlags::DIAG, "diag"),
        (KernelDiagFlags::PAGER, "pager"),
        (KernelDiagFlags::INVLS, "invls"),
        (KernelDiagFlags::WAKE, "wake"),
        (KernelDiagFlags::FAULT, "fault"),
    ]
    .iter()
    .filter(|(f, _)| flags.contains(*f))
    .map(|(_, n)| *n)
    .collect();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(",")
    }
}

fn main() -> ExitCode {
    let args = Args::parse();

    let mut flags = SysCtrlFlags::empty();
    if args.no_wait {
        flags |= SysCtrlFlags::NO_WAIT;
    }
    if args.verbose {
        flags |= SysCtrlFlags::VERBOSE;
    }
    let timeout = args.timeout.map(Duration::from_millis);

    // Not a sys_ctrl call at all unless --immediate was given: the request goes to the monitor,
    // and init's watcher makes the kernel call.
    if let Command::Shutdown {
        immediate: false, ..
    } = &args.cmd
    {
        return match monitor_api::monitor_state_or(MonitorState::SHUTDOWN) {
            Ok(_) => {
                println!("shutdown requested");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("ctl: shutdown: {}", e);
                ExitCode::FAILURE
            }
        };
    }

    let (cmd, arg1, arg2) = match &args.cmd {
        Command::DebugDump => (SysCtrlCmd::DebugDump, 0, 0),
        Command::SyncAll => (SysCtrlCmd::SyncAll, 0, 0),
        Command::ZeroAll => (SysCtrlCmd::ZeroAll, 0, 0),
        Command::SetDiag { on, off } => (SysCtrlCmd::SetDiag, mask(on), mask(off)),
        Command::ReapAll => (SysCtrlCmd::ReapAll, 0, 0),
        Command::Shutdown { code, .. } => (SysCtrlCmd::Shutdown, *code as u64, 0),
    };

    match sys_ctrl(cmd, timeout, flags, arg1, arg2, 0) {
        Ok(val) => {
            match &args.cmd {
                // Said explicitly because the dump does not come back through this process: it
                // goes to the kernel console, which is not necessarily the terminal you are on.
                Command::DebugDump => println!("debug dump written to the kernel console"),
                Command::SyncAll => println!("synced {} objects, flushed the store", val),
                Command::ZeroAll => println!("zeroed {} bytes ({} KiB)", val, val / 1024),
                Command::SetDiag { .. } => println!("kernel diagnostics: {}", describe(val)),
                Command::ReapAll => println!("reaped {} objects and threads", val),
                // Unreachable: the kernel does not return from this one.
                Command::Shutdown { .. } => println!("shutdown returned unexpectedly"),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ctl: {:?}: {}", cmd, e);
            // A timeout is not "nothing happened" -- the kernel did as much as it could and
            // logged how much, and that count is not carried back through the error.
            if e == TwzError::TIMED_OUT {
                eprintln!("ctl: partial progress was logged to the kernel console");
            }
            ExitCode::FAILURE
        }
    }
}
