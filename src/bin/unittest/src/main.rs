use std::{
    io::{BufRead, Read, Seek},
    sync::OnceLock,
    time::Instant,
};

use twizzler_abi::syscall::{
    sys_kernel_console_write, KernelConsoleSource, KernelConsoleWriteFlags,
};
use unittest_report::{Report, ReportInfo, TestResult, TestStatus};

static RESULT: OnceLock<Report> = OnceLock::new();

/// Emit a REPORT line in a single console write.
///
/// The host matches this line with `strip_prefix("REPORT ")`, so it has to arrive whole. `println!`
/// cannot promise that: stdout is line-buffered at 1KiB and a full report runs to several KiB, so
/// it left as half a dozen writes with room between them. Kernel logging landed inside one, which
/// the host then could not parse -- a run whose tests had all passed was recorded as "no test
/// report". One `sys_kernel_console_write` of the whole buffer fixes the guest's half of that: the
/// kernel passes the slice to the uart in one call, holding the port lock across it.
///
/// It was only half. The kernel's own prints were the fragmented ones -- `core::fmt` calls
/// `write_str` per format fragment, and each became its own locked write -- so this line could not
/// be split but could land *inside* one of theirs. Same corruption, other side, and it recurred:
/// three runs of a 4661-run sweep passed every test and were recorded as "no test report", one of
/// them inside a line from `emerglogln`, which holds no lock at all and cannot be made to. Kernel
/// messages are now single writes too (`kernel/src/log.rs`, `LineWriter`).
///
/// The leading newline covers the last way to land mid-line, which no amount of atomicity fixes:
/// being *appended* to a line someone else has not finished. libtest writes `test <name> ... ` and
/// its `ok` as two writes, so anything emitted between them shares that line -- harmless for the
/// heartbeats, fatal for this one. Starting our own line costs a blank line per report.
fn emit_report(report: &Report) {
    let line = format!("\nREPORT {}\n", serde_json::to_string(report).unwrap());
    sys_kernel_console_write(
        KernelConsoleSource::Console,
        line.as_bytes(),
        KernelConsoleWriteFlags::DONT_BUFFER,
    );
}

/// Directories a test binary may live in, in search order.
///
/// `#[test]` binaries are staged on the disk (`xtask`'s `TEST_DIR_ON_DISK`) rather than in the
/// initrd, because the initrd is read whole through UEFI block I/O at boot whether or not a test
/// runs. Standalone `test-programs` are ordinary userspace binaries and land in `bin/`. `/initrd`
/// stays last as the fallback: an image built before the move still works, and so does a boot where
/// the disk never came up -- running the suite the slow way beats reporting 50 spawn failures.
const SEARCH_DIRS: &[&str] = &["/pkg/twizzler/test", "/pkg/twizzler/bin", "/initrd"];

/// First directory in [`SEARCH_DIRS`] that actually holds `name`.
///
/// Falls back to the primary location when nothing matches, so the spawn error names where the
/// binary was supposed to be rather than wherever the search happened to end.
fn resolve(name: &str) -> String {
    SEARCH_DIRS
        .iter()
        .map(|dir| format!("{}/{}", dir, name))
        .find(|path| std::fs::metadata(path).is_ok())
        .unwrap_or_else(|| format!("{}/{}", SEARCH_DIRS[0], name))
}

fn try_bench(path: &str) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    println!("starting benchmarking ({})", path);
    let start = Instant::now();
    for line in std::io::BufReader::new(file).lines() {
        if let Ok(line) = &line {
            if line.contains("\u{0000}") {
                continue;
            }
            if !line.is_ascii() {
                continue;
            }
            println!("STARTING {}", line);
            // A line is `<binary> [filter]...`: everything after the binary name is passed to
            // the harness after `--bench`, so `--bench "sysbench page_fault_zero_fill"` runs one
            // bench in a fresh boot instead of the whole suite.
            let mut tokens = line.split_whitespace();
            let line = tokens.next().unwrap_or_default().to_owned();
            let filters: Vec<&str> = tokens.collect();
            // Benches are matched by name prefix rather than looked up exactly, so this searches
            // the same directories as `resolve` instead of just the initrd. A directory that does
            // not exist is skipped: `/pkg/twizzler/test` is absent on an image built before the
            // tests moved onto the disk.
            let mut possibles = Vec::new();
            let underscored = line.replace("-", "_");
            for dir in SEARCH_DIRS {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
                for exe in entries.flatten() {
                    let exe = exe.file_name().to_string_lossy().into_owned();
                    // One `if` per prefix, as this was written, double-pushes every name that has
                    // no dash to replace -- and then runs that benchmark twice.
                    if exe.starts_with(line.as_str()) || exe.starts_with(&underscored) {
                        possibles.push(format!("{}/{}", dir, exe));
                    }
                }
            }
            // bench_bins lines are exact staged file names; stale same-crate binaries with older
            // cargo hashes also prefix-match and would re-run (polluting kernel state) before the
            // current one. Keep only the exact match when one exists -- a user-typed
            // `--bench <prefix>` still falls through to the prefix scan.
            if possibles.iter().any(|p| p.ends_with(&format!("/{}", line))) {
                possibles.retain(|p| p.ends_with(&format!("/{}", line)));
                possibles.truncate(1);
            }
            for (i, exe) in possibles.iter().enumerate() {
                let mut cmd = std::process::Command::new(exe);
                cmd.args(["--bench"]);
                cmd.args(&filters);
                if let Ok(mut test_comp) = cmd.spawn() {
                    test_comp.wait().unwrap();
                } else {
                    if i == possibles.len() - 1 {
                        eprintln!("failed to start {}", exe);
                    }
                }
            }
        }
    }
    let dur = Instant::now() - start;
    println!("unittest: benches finished in {:?}", dur);
}

/// A test binary that has been spawned but not yet waited on.
struct Pending {
    path: String,
    started: Instant,
    /// The spawn error is carried instead of reported here so a failure to start is graded in
    /// [`finish_one`] alongside every other outcome.
    child: std::io::Result<std::process::Child>,
}

/// Spawn `name` with `args` and `envs` without waiting for it. See [`resolve`] for where `name` is
/// looked up.
fn start_one(name: &str, args: &[&str], envs: &[(&str, &str)]) -> Pending {
    let path = resolve(name);
    println!("STARTING {}", path);
    let mut cmd = std::process::Command::new(&path);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let started = Instant::now();
    let child = cmd.spawn();
    Pending {
        path,
        started,
        child,
    }
}

/// Wait for a [`Pending`] test and grade it by exit status.
fn finish_one(pending: Pending) -> TestResult {
    let Pending {
        path,
        started,
        child,
    } = pending;
    // Never unwrap the wait: a panic here would discard every result collected so far and
    // the host would report "no report" instead of the actual failure.
    let status = match child.and_then(|mut test_comp| test_comp.wait()) {
        Ok(st) if st.success() => TestStatus::Passed,
        Ok(st) => TestStatus::Failed {
            code: st.code().unwrap_or(-1),
        },
        Err(e) => TestStatus::SpawnFailed { err: e.to_string() },
    };
    println!("FINISHED {}: {}", path, status);
    TestResult {
        name: path,
        status,
        duration: started.elapsed(),
    }
}

/// Spawn `name`, wait for it, and turn the result into a `TestResult`. Used for every test that is
/// not in [`EARLY_START`].
fn run_one(name: &str, args: &[&str], envs: &[(&str, &str)]) -> TestResult {
    finish_one(start_one(name, args, envs))
}

/// Test binaries started before the rest of the suite and collected after it.
///
/// `net_test` is wait-bound rather than CPU-bound -- it sits on peer connects and multi-second idle
/// windows -- so run in turn it leaves the machine idle for most of the ~5s (smp4) to ~8s (smp1) it
/// takes, against a suite that is otherwise ~3.5s of work. Overlapping it puts the whole suite in
/// its shadow: 8.9s -> 5.2s on smp4, 16.4s -> 9.6s on smp1.
///
/// A test belongs here only if it is safe beside an arbitrary other test: `net_test` binds fixed
/// ports (7701+) that nothing else in the suite touches.
///
/// Adding a *second* entry is where this stops paying. `twizzler_queue_raw` is the other slow test
/// (5.2s on smp1, where cross-thread wake latency dominates) and starting it early too measured
/// *worse* on smp1 -- 12.0s, because the two then contend for the single CPU and inflate each other
/// past what either costs alone. Sequential, it fits inside `net_test`'s window for free.
const EARLY_START: &[&str] = &["net_test"];

/// Crate name of a test binary, i.e. `net_test` from `net_test-3237d80032434c66`.
///
/// Split from the right, since only the trailing `-<hash>` is guaranteed separator; matching whole
/// names this way keeps `net_test` distinct from `net_test_peer`, which a prefix test would not.
fn crate_name(bin: &str) -> &str {
    bin.rsplit_once('-')
        .map(|(name, _hash)| name)
        .unwrap_or(bin)
}

/// Parse `/initrd/standalone_test_bins`: one entry per line, `<binary-name> [arg]...`. Missing
/// file just means no standalone programs were opted in (`[workspace.metadata] test-programs`),
/// not an error.
fn read_standalone_entries(path: &str) -> Vec<(String, Vec<String>)> {
    let Ok(file) = std::fs::File::open(path) else {
        return vec![];
    };
    std::io::BufReader::new(file)
        .lines()
        .filter_map(|line| line.ok())
        .filter(|line| !line.contains('\u{0000}') && line.is_ascii() && !line.trim().is_empty())
        .map(|line| {
            let mut tokens = line.split_whitespace().map(String::from);
            let name = tokens.next().unwrap_or_default();
            let args = tokens.collect();
            (name, args)
        })
        .collect()
}

fn main() {
    println!("unittest: starting");
    try_bench("/initrd/bench_bins");
    try_bench("/initrd/bench_bin");
    let Ok(mut file) = std::fs::File::open("/initrd/test_bins")
        .inspect_err(|e| eprintln!("failed to open test bins: {}", e))
    else {
        return;
    };

    // Detached: it answers the host's liveness polls while the suite runs, but nothing waits on it
    // -- see the report push below.
    std::thread::spawn(|| io_heartbeat());

    let mut reports = vec![];
    let start = Instant::now();
    println!("unittest file len: {}", file.metadata().unwrap().len());
    let mut v = Vec::new();
    let data = file.read_to_end(&mut v).unwrap();
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    println!("unittest: read {} bytes from test_bins", data);
    let lines: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .filter_map(|line| line.ok())
        .filter(|line| !line.contains('\u{0000}') && line.is_ascii() && !line.trim().is_empty())
        .collect();

    // Start the wait-bound tests before anything else, so the rest of the suite runs inside the
    // windows they spend blocked. They are collected below, after everything sequential is done.
    let early: Vec<Pending> = lines
        .iter()
        .filter(|line| EARLY_START.contains(&crate_name(line.as_str())))
        .map(|line| start_one(line.as_str(), &["--test"], &[("TWZ_TEST_MODE", "1")]))
        .collect();
    println!(
        "unittest: started {} test(s) in the background",
        early.len()
    );

    for line in lines
        .iter()
        .filter(|line| !EARLY_START.contains(&crate_name(line.as_str())))
    {
        reports.push(run_one(line, &["--test"], &[("TWZ_TEST_MODE", "1")]));
    }

    for (name, args) in read_standalone_entries("/initrd/standalone_test_bins") {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        reports.push(run_one(&name, &args, &[]));
    }

    for pending in early {
        reports.push(finish_one(pending));
    }

    let dur = Instant::now() - start;
    println!("unittest: tests finished");
    let info = ReportInfo {
        time: dur,
        tests: reports,
    };
    let failed = info.failed();
    RESULT.set(Report::ready(info)).unwrap();

    // Push the report as soon as it exists rather than waiting to be asked. The host polls
    // `status` once per heartbeat (15s), so waiting for it idled every run for half that on
    // average. `io_heartbeat` still answers polls, and the host keeps the first REPORT it sees, so
    // a poll racing this line just produces a duplicate.
    emit_report(RESULT.get().unwrap());

    // Exit nonzero so init can hand a real code to sys_debug_shutdown; that is the backstop the
    // host falls back on when the REPORT channel produces nothing. Only do this once the report
    // has been delivered above.
    if failed > 0 {
        eprintln!("unittest: {} test binaries failed", failed);
        std::process::exit(1);
    }
}

fn io_heartbeat() {
    let mut buf = String::new();
    loop {
        // Clear before reading, not after: read_line appends, and the unknown-command path used
        // to skip the clear entirely, so a single stray console byte would corrupt every
        // subsequent line and break the status protocol for the rest of the run.
        buf.clear();
        match std::io::stdin().read_line(&mut buf) {
            // Ok(0) is EOF; looping on it would spin forever.
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        match buf.as_str().trim() {
            "status" => {
                if let Some(report) = RESULT.get() {
                    println!("unittest: creating report");
                    emit_report(report);
                    return;
                } else {
                    emit_report(&Report::pending());
                }
            }
            _ => {
                println!("!! unknown command: {}", buf);
            }
        }
    }
}
