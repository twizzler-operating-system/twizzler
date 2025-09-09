use std::{io::BufRead, sync::OnceLock, time::Instant};

use unittest_report::{Report, ReportInfo, TestResult};

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
                if let Ok(test_comp) = monitor_api::CompartmentLoader::new(
                    &exe,
                    &exe,
                    monitor_api::NewCompartmentFlags::empty(),
                )
                .args(&[exe.as_str(), "--bench"])
                .load()
                {
                    let mut flags = test_comp.info().flags;
                    while !flags.contains(monitor_api::CompartmentFlags::EXITED) {
                        flags = test_comp.wait(flags);
                    }
                } else {
                    if i == possibles.len() - 1 {
                        eprintln!("failed to start {}", line);
                    }
                }
            }
        }
    }
    let dur = Instant::now() - start;
    println!("unittest: benches finished in {:?}", dur);
}

fn main() {
    try_bench("/initrd/bench_bins");
    try_bench("/initrd/bench_bin");
    let Ok(file) = std::fs::File::open("/initrd/test_bins")
        .inspect_err(|e| eprintln!("failed to open test bins: {}", e))
    else {
        return;
    };

    let heartbeat_thread = std::thread::spawn(|| io_heartbeat());

    let mut reports = vec![];
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
            if let Ok(test_comp) = monitor_api::CompartmentLoader::new(
                line,
                line,
                monitor_api::NewCompartmentFlags::empty(),
            )
            .args(&[line.as_str(), "--test"])
            .load()
            {
                let mut flags = test_comp.info().flags;
                while !flags.contains(monitor_api::CompartmentFlags::EXITED) {
                    flags = test_comp.wait(flags);
                }
                reports.push(TestResult {
                    name: line.clone(),
                    passed: true,
                });
            } else {
                reports.push(TestResult {
                    name: line.clone(),
                    passed: false,
                });
            }
        }
    }
    let dur = Instant::now() - start;
    println!("unittest: tests finished, waiting for status request");
    RESULT
        .set(Report::ready(ReportInfo {
            time: dur,
            tests: reports,
        }))
        .unwrap();
    heartbeat_thread.join().unwrap();
}

fn io_heartbeat() {
    let mut buf = String::new();
    while let Ok(_) = std::io::stdin().read_line(&mut buf) {
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
        buf.clear();
    }
}
