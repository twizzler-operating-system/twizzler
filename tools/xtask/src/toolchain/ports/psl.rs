use std::{
    fs::OpenOptions, io::Write, os::linux::raw::stat, path::Path, process::Command,
    thread::available_parallelism,
};

use futures::executor::block_on;
use reqwest::Client;

use crate::{toolchain::download_file, triple::Triple};

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building libpsl for {}", triple);

    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let client = Client::new();
    let url = "https://github.com/rockdaboot/libpsl/releases/download/0.21.5/libpsl-0.21.5.tar.gz";

    let cont_dir = Path::new("toolchain/build/ports/libpsl");
    std::fs::create_dir_all(&cont_dir)?;
    let cont_dir = cont_dir.canonicalize()?;
    let tar_file = cont_dir.join("libpsl-0.21.5.tar.gz");
    if !std::fs::exists(&tar_file)? {
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_file(
                &client,
                url,
                tar_file.display().to_string().as_str(),
            ))?;
    }

    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg("libpsl-0.21.5.tar.gz")
        .current_dir("toolchain/build/ports/libpsl")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract libpsl");
    }

    let src_dir = Path::new("toolchain/build/ports/libpsl/libpsl-0.21.5").canonicalize()?;
    let build_dir = Path::new("toolchain/build/ports/libpsl/build").join(triple.to_string());
    let install_dir = Path::new("toolchain/install/sysroots").join(&triple.to_string());
    std::fs::create_dir_all(&install_dir)?;
    std::fs::create_dir_all(&build_dir)?;
    let install_dir = install_dir.canonicalize()?;
    let build_dir = build_dir.canonicalize()?;

    let bin_dir = Path::new("toolchain/install/bin").canonicalize()?;

    let mut cmd = Command::new(src_dir.join("configure"));
    cmd.current_dir(&build_dir);

    cmd.arg("--host")
        .arg(triple.to_string())
        .arg("--target")
        .arg(triple.to_string())
        .arg("--build")
        .arg(crate::toolchain::guess_host_triple().unwrap())
        .arg("--prefix=/pkg/libpsl")
        .arg("--enable-shared")
        .arg("--enable-optimizations");
    cmd.env("DESTDIR", &install_dir);

    let cflags = format!(
        "-target {} --sysroot {} -fPIC",
        triple,
        sysroot_dir.display()
    );

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
        anyhow::bail!("failed to configure libpsl");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(&build_dir)
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to build libpsl");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(&build_dir)
        .arg("install")
        .arg(format!("DESTDIR={}", sysroot_dir.display()))
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to install libpsl");
    }

    Ok(())
}
