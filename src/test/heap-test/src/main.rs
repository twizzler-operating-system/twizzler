//! The C heap stays bounded under churn: large malloc/free, varied sizes, realloc growth, and
//! threads that allocate and are joined (each used to keep its allocator heap). Runs standalone
//! (graded by exit code via test-programs); the C program is this crate's other binary.

use std::process::{Command, ExitCode};

const CASES: &[(&str, &str)] = &[
    ("malloc", "512"),
    ("sizes", "512"),
    ("realloc", "256"),
    ("threads", "512"),
];

fn main() -> ExitCode {
    let mut failed = 0;
    for (mode, rounds) in CASES {
        let status = Command::new("heap-test-c").args([mode, rounds]).status();
        let ok = matches!(&status, Ok(s) if s.success());
        println!("heap-test: {mode}: {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            println!("heap-test: {mode}: {status:?}");
            failed += 1;
        }
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
