use std::{process::Command, thread::available_parallelism};

use reqwest::Client;

use crate::{
    toolchain::{bootstrap::setup_logfile, download_file},
    triple::Triple,
};

const ZLIB_URL: &str = "https://zlib.net/zlib-1.3.2.tar.gz";

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building zlib for {}", triple);

    let cont_dir = std::path::Path::new("toolchain/install/build/ports/zlib");
    std::fs::create_dir_all(&cont_dir)?;
    let install_dir = std::path::Path::new("toolchain/install/sysroots").join(&triple.to_string());
    std::fs::create_dir_all(&install_dir)?;
    let install_dir = install_dir.canonicalize()?;
    if !std::fs::exists("toolchain/install/build/ports/zlib/zlib-1.3.2.tar.gz")? {
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_file(
                &client,
                ZLIB_URL,
                "toolchain/install/build/ports/zlib/zlib-1.3.2.tar.gz",
            ))?;
    }

    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg("zlib-1.3.2.tar.gz")
        .current_dir("toolchain/install/build/ports/zlib")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract zlib");
    }

    let log = setup_logfile("ports/zlib", "xtask-configure", Some(triple))?;
    let build_dir =
        std::path::Path::new("toolchain/install/build/ports/zlib/build").join(triple.to_string());
    let source_dir = std::path::Path::new("toolchain/install/build/ports/zlib").join("zlib-1.3.2");

    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;
    let source_dir = source_dir.canonicalize()?;
    let bin_dir = std::path::Path::new("toolchain/install/bin").canonicalize()?;
    let sysroot_dir = std::path::Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let mut cmd = Command::new(source_dir.join("configure"));
    cmd.stdout(log).current_dir(&build_dir);

    cmd.arg("--prefix=/pkg/zlib");

    let cflags = format!("-target {} --sysroot {}", triple, sysroot_dir.display());

    cmd.env("PKG_CONFIG", "");
    cmd.env("CFLAGS", &cflags);
    cmd.env("CXXFLAGS", &cflags);
    cmd.env("LDFLAGS", &cflags);
    cmd.env("CC", bin_dir.join("clang").display().to_string());
    cmd.env("CXX", bin_dir.join("clang++").display().to_string());
    cmd.env("LD", bin_dir.join("clang").display().to_string());
    let mut lds = bin_dir.join("clang").display().to_string();
    lds.push_str(" -shared");
    cmd.env("LDSHARED", lds);
    cmd.env("AR", bin_dir.join("llvm-ar").display().to_string());
    cmd.env("RANLIB", bin_dir.join("llvm-ranlib").display().to_string());

    let mut ch = cmd.spawn()?;
    if !ch.wait()?.success() {
        anyhow::bail!("failed to configure zlib");
    }
    let log = setup_logfile("ports/zlib", "xtask-make", Some(triple))?;
    let mut cmd = Command::new("make");
    cmd.arg("-j")
        .arg(available_parallelism().unwrap().to_string());
    cmd.stdout(log).current_dir(&build_dir);
    cmd.env("DESTDIR", install_dir);

    cmd.arg("install");

    let mut ch = cmd.spawn()?;
    if !ch.wait()?.success() {
        anyhow::bail!("failed to build zlib");
    }

    Ok(())
}
