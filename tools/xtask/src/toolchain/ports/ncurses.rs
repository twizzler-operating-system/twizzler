use std::{process::Command, thread::available_parallelism};

use reqwest::Client;

use crate::{
    toolchain::{bootstrap::setup_logfile, download_file},
    triple::Triple,
};

const NCURSES_URL: &str = "https://github.com/mirror/ncurses/archive/refs/tags/v6.4.tar.gz";

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building ncurses for {}", triple);

    let cont_dir = std::path::Path::new("toolchain/build/ports/ncurses");
    std::fs::create_dir_all(&cont_dir)?;
    if !std::fs::exists("toolchain/build/ports/ncurses/ncurses-6.4.tar.gz")? {
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_file(
                &client,
                NCURSES_URL,
                "toolchain/build/ports/ncurses/ncurses-6.4.tar.gz",
            ))?;
    }

    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg("ncurses-6.4.tar.gz")
        .current_dir("toolchain/build/ports/ncurses")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract ncurses");
    }
    let log = setup_logfile("ports/ncurses", "xtask-configure", Some(triple))?;

    let build_dir =
        std::path::Path::new("toolchain/build/ports/ncurses/build").join(triple.to_string());
    let source_dir = std::path::Path::new("toolchain/build/ports/ncurses").join("ncurses-6.4");
    let install_dir = std::path::Path::new("toolchain/install/sysroots").join(&triple.to_string());
    std::fs::create_dir_all(&install_dir)?;
    std::fs::create_dir_all(&build_dir)?;
    let install_dir = install_dir.canonicalize()?;
    let build_dir = build_dir.canonicalize()?;
    let source_dir = source_dir.canonicalize()?;
    let bin_dir = std::path::Path::new("toolchain/install/bin").canonicalize()?;
    let sysroot_dir = std::path::Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let mut cmd = Command::new(source_dir.join("configure"));
    cmd.current_dir(&build_dir).stdout(log).arg("--prefix=/");

    cmd.arg("--host")
        .arg(triple.to_string())
        .arg("--target")
        .arg(triple.to_string())
        .arg("--build")
        .arg(crate::toolchain::guess_host_triple().unwrap())
        .arg("--prefix=/pkg/ncurses")
        .arg("--enable-shared")
        .arg("--program-prefix=")
        .arg("--with-install-prefix");
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
        anyhow::bail!("failed to configure ncurses");
    }

    let log = setup_logfile("ports/ncurses", "xtask-make", Some(triple))?;
    let mut cmd = Command::new("make");
    cmd.arg("-j")
        .arg(available_parallelism().unwrap().to_string());
    cmd.stdout(log).current_dir(&build_dir);
    cmd.env("DESTDIR", install_dir);

    cmd.arg("install");

    let mut ch = cmd.spawn()?;
    if !ch.wait()?.success() {
        anyhow::bail!("failed to build ncurses");
    }

    Ok(())
}
