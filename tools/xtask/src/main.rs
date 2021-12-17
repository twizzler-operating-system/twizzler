use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

type DynError = Box<dyn std::error::Error>;

fn main() {
    if let Err(e) = try_main() {
        // eprintln!("{}", e);
        std::process::exit(101);
    }
}

fn try_main() -> Result<(), DynError> {
    let task = env::args().nth(1);
    match task.as_ref().map(|it| it.as_str()) {
        Some("build-all") => build_all()?,
        Some("check-all") => check_all()?,
        _ => print_help(),
    }
    Ok(())
}

fn print_help() {
    println!("xtask help TODO")
}

fn check_all() -> Result<(), DynError> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status1 = Command::new(cargo)
        .current_dir("src/kernel")
        .args(&["check", "--message-format=json"])
        .status()?;
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status2 = Command::new(cargo)
        .args(&[
            "check",
            "--message-format=json",
            "--workspace",
            "--exclude",
            "rustkernel",
        ])
        .status()?;

    if !status1.success() {
        Err("cargo check failed")?;
    }
    if !status2.success() {
        Err("cargo check failed")?;
    }
    Ok(())
}

fn build_all() -> Result<(), DynError> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(cargo)
        .current_dir("src/kernel")
        .args(&["build"])
        .status()?;

    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(cargo)
        .args(&["build", "--workspace", "--exclude", "rustkernel"])
        .status()?;

    if !status.success() {
        Err("cargo build failed")?;
    }
    Ok(())
}
