use std::{
    io::{BufRead, Read, Seek},
    sync::OnceLock,
    time::Instant,
};

use unittest_report::{Report, ReportInfo, TestResult, TestStatus};

static RESULT: OnceLock<Report> = OnceLock::new();

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
            let mut possibles = Vec::new();
            for exe in std::fs::read_dir("/initrd").unwrap() {
                if exe
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(line)
                {
                    possibles.push(format!(
                        "/initrd/{}",
                        exe.as_ref().unwrap().file_name().to_string_lossy()
                    ));
                }
                if exe
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&line.replace("-", "_"))
                {
                    possibles.push(format!(
                        "/initrd/{}",
                        exe.as_ref().unwrap().file_name().to_string_lossy()
                    ));
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

fn main() {
    println!("unittest: starting");
    try_bench("/initrd/bench_bins");
    try_bench("/initrd/bench_bin");
    let Ok(mut file) = std::fs::File::open("/initrd/test_bins")
        .inspect_err(|e| eprintln!("failed to open test bins: {}", e))
    else {
        return;
    };

    let heartbeat_thread = std::thread::spawn(|| io_heartbeat());

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
            let line = &format!("/initrd/{}", line);
            println!("STARTING {}", line);
            let mut cmd = std::process::Command::new(line);
            cmd.env("TWZ_TEST_MODE", "1");
            cmd.args(["--test"]);
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
            println!("FINISHED {}: {}", line, status);
            reports.push(TestResult {
                name: line.clone(),
                status,
                duration: started.elapsed(),
            });
        }
    }
    let dur = Instant::now() - start;
    println!("unittest: tests finished, waiting for status request");
    let info = ReportInfo {
        time: dur,
        tests: reports,
    };
    let failed = info.failed();
    RESULT.set(Report::ready(info)).unwrap();
    heartbeat_thread.join().unwrap();

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
