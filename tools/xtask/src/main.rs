use std::{env, process::Command};

use cargo_metadata::MetadataCommand;

type DynError = Box<dyn std::error::Error>;

fn main() {
    if let Err(_e) = try_main() {
        // eprintln!("{}", e);
        std::process::exit(101);
    }
}

fn try_main() -> Result<(), DynError> {
    let task = env::args().nth(1);
    match task.as_ref().map(|it| it.as_str()) {
        Some("build-all") => build_all()?,
        Some("check-all") => check_all()?,
        Some("make-disk") => make_disk()?,
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

    if !status.success() {
        Err("cargo build failed")?;
    }
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(cargo)
        .args(&["build", "--workspace", "--exclude", "rustkernel"])
        .status()?;

    if !status.success() {
        Err("cargo build failed")?;
    }
    Ok(())
}

fn make_disk() -> Result<(), DynError> {
    let path = "Cargo.toml";
    let _meta = MetadataCommand::new().manifest_path(path).exec().unwrap();
    build_all()?;
    let status = Command::new("target/debug/image_builder")
        .arg("target/x86_64-pc-none/debug/rustkernel")
        .status()?;

    if !status.success() {
        Err("disk image creation failed")?;
    }
    Ok(())
}
