use std::{
    fs::{File, OpenOptions},
    path::Path,
};

use super::BootstrapOptions;
use crate::{
    build::do_post_toolchain_runtime_build,
    toolchain::{
        bootstrap::prep::generate_config_toml, compress_toolchain, generate_tag, prune_bins,
        prune_toolchain,
    },
    triple::{all_possible_platforms, Triple},
};

mod libc;
mod llvm;
mod prep;
mod rust;

pub fn setup_logfile(step: &str, substep: &str, triple: Option<&Triple>) -> anyhow::Result<File> {
    let logname = format!("{}.log", substep);

    let logdir = Path::new("toolchain/build").join(step);
    let logdir = if let Some(triple) = triple {
        logdir.join(triple.to_string())
    } else {
        logdir
    };

    let logfile = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .read(true)
        .open(logdir.join(logname))?;

    println!(
        "==> Performing {}: {} {}",
        step,
        substep,
        if let Some(triple) = triple {
            format!("(for {})", triple)
        } else {
            format!("")
        }
    );

    Ok(logfile)
}

pub(crate) fn do_bootstrap(cli: BootstrapOptions) -> anyhow::Result<()> {
    println!(
        "Starting bootstrap with steps: {}",
        cli.step
            .as_ref()
            .map(|s| s.join(","))
            .unwrap_or("all".to_string())
    );

    let tag = generate_tag()?;
    let toolchain_path = Path::new("toolchain").join(&tag);
    std::fs::create_dir_all(&toolchain_path)?;
    if std::fs::symlink_metadata(Path::new("toolchain/install")).is_ok_and(|r| r.is_dir()) {
        let _ = fs_extra::remove_items(&[Path::new("toolchain/install")]);
    }
    let _ = std::fs::remove_file("toolchain/install");
    std::os::unix::fs::symlink(&tag, "toolchain/install")?;

    if cli.has_step("prep") {
        prep::setup_build(&cli)?;
    }

    if cli.has_step("llvm") {
        llvm::build_llvm(&cli)?;
        llvm::build_lld(&cli)?;
    }

    for triple in all_possible_platforms() {
        if cli.has_step("crt") {
            libc::install_headers(&cli, &triple)?;
            llvm::build_runtimes(&cli, &triple)?;
        }

        if cli.has_step("libc") {
            libc::build_libc(&cli, &triple)?;
        }

        if cli.has_step("libcxx") {
            libc::build_libcxx(&cli, &triple)?;
        }
    }

    let path = std::env::var("PATH").unwrap();
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}:{}",
            std::fs::canonicalize("toolchain/install/bin")
                .unwrap()
                .to_string_lossy(),
            std::fs::canonicalize("toolchain/install/python/bin")
                .unwrap()
                .to_string_lossy(),
            path
        ),
    );

    let sysroots = Path::new("toolchain/install/sysroots").canonicalize()?;
    std::env::set_var("TWIZZLER_ABI_SYSROOTS", sysroots);

    if cli.has_step("rust") {
        println!("generating rust bootstrap.config file");
        let _ = std::fs::remove_file("toolchain/src/rust/bootstrap.toml");
        generate_config_toml(&cli)?;
        println!("starting rust build");
        rust::build_rust(&cli)?;
    }

    if cli.native {
        return Ok(());
    }

    if !cli.skip_prune {
        prune_toolchain()?;
    }

    if cli.has_step("rt") {
        println!("building runtimes");
        do_post_toolchain_runtime_build(&cli)?;
    }

    if cli.compress {
        println!("pruning binaries");
        prune_bins()?;
        println!("compressing toolchain");
        compress_toolchain()?;
    }

    println!("ready!");
    Ok(())
}
