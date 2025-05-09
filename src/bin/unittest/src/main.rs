use std::{io::BufRead, sync::OnceLock, time::Instant};

use unittest_report::{Report, ReportInfo, TestResult};

static RESULT: OnceLock<Report> = OnceLock::new();

fn main() {
    let file = std::fs::File::open("/initrd/test_bins").expect("no test binaries specified");

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
