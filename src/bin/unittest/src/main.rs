use std::{
    io::{BufRead, Read, Seek},
    sync::OnceLock,
    time::Instant,
};

use unittest_report::{Report, ReportInfo, TestResult, TestStatus};

static RESULT: OnceLock<Report> = OnceLock::new();

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
            for (i, exe) in possibles.iter().enumerate() {
                let mut cmd = std::process::Command::new(exe);
                cmd.args(["--bench"]);
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

/// Spawn `name` with `args` and `envs`, and turn the result into a `TestResult`. Shared by the
/// `#[test]`-binary loop and the standalone-program loop: both are just "run a binary, grade it by
/// exit status." See [`resolve`] for where `name` is looked up.
fn run_one(name: &str, args: &[&str], envs: &[(&str, &str)]) -> TestResult {
    let path = resolve(name);
    println!("STARTING {}", path);
    let mut cmd = std::process::Command::new(&path);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let started = Instant::now();
    // Never unwrap the wait: a panic here would discard every result collected so far and
    // the host would report "no report" instead of the actual failure.
    let status = match cmd.spawn().and_then(|mut test_comp| test_comp.wait()) {
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
    for line in std::io::BufReader::new(file).lines() {
        println!("got line: {:?}", line);
        if let Ok(line) = &line {
            if line.contains("\u{0000}") {
                continue;
            }
            if !line.is_ascii() {
                continue;
            }
            reports.push(run_one(line, &["--test"], &[("TWZ_TEST_MODE", "1")]));
        }
    }

    for (name, args) in read_standalone_entries("/initrd/standalone_test_bins") {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        reports.push(run_one(&name, &args, &[]));
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
    println!(
        "REPORT {}",
        serde_json::to_string(RESULT.get().unwrap()).unwrap()
    );

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
                    println!("REPORT {}", serde_json::to_string(report).unwrap());
                    return;
                } else {
                    println!(
                        "REPORT {}",
                        serde_json::to_string(&Report::pending()).unwrap()
                    );
                }
            }
            _ => {
                println!("!! unknown command: {}", buf);
            }
        }
    }
}
