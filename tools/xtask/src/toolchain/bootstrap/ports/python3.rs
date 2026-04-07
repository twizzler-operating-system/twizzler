use std::{path::Path, process::Command, thread::available_parallelism};

use crate::{
    toolchain::{guess_host_triple, BootstrapOptions},
    triple::Triple,
};

pub fn install(cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    let src_dir = Path::new("src/ports/python/Python-3.14.3").canonicalize()?;
    let build_dir = Path::new("toolchain/build/ports/python").join(triple.to_string());
    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;
    let bin_dir = Path::new("toolchain/install/bin").canonicalize()?;

    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    let mut cmd = Command::new(src_dir.join("configure"));
    cmd.current_dir(&build_dir)
        .arg("--host")
        .arg(triple.to_string())
        .arg("--target")
        .arg(triple.to_string())
        .arg("--enable-shared")
        .arg("--with-mimalloc=no")
        .arg("--with-pymalloc=no")
        .arg("--prefix=/usr")
        .arg("--build")
        .arg(guess_host_triple().unwrap())
        //.arg("--enable-optimizations")
        .arg("--with-build-python=python3.14")
        .arg("ac_cv_file__dev_ptmx=no")
        .arg("ac_cv_file__dev_ptc=no")
        .arg("ac_cv_func_sched_setscheduler=no")
        .arg("--disable-ipv6");

    let cflags = format!("-target {} --sysroot {}", triple, sysroot_dir.display());

    cmd.env("PKG_CONFIG", "");
    cmd.env("CFLAGS", &cflags);
    cmd.env("CXXFLAGS", &cflags);
    cmd.env("LDFLAGS", &cflags);
    cmd.env("CC", bin_dir.join("clang").display().to_string());
    cmd.env("CXX", bin_dir.join("clang++").display().to_string());
    cmd.env("LD", bin_dir.join("clang").display().to_string());
    cmd.env("AR", bin_dir.join("llvm-ar").display().to_string());
    cmd.env("RANLIB", bin_dir.join("llvm-ranlib").display().to_string());
    cmd.env("BLDSHARED", format!("{}/clang -shared", bin_dir.display()));
    cmd.env("LDSHARED", format!("{}/clang -shared", bin_dir.display()));

    let mut ch = cmd.spawn()?;
    if !ch.wait()?.success() {
        anyhow::bail!("failed to configure python");
    }

    let mut cmd = Command::new("make");
    cmd.arg("-j")
        .arg(available_parallelism().unwrap().to_string());
    cmd.current_dir(&build_dir);
    cmd.env("DESTDIR", sysroot_dir);

    cmd.arg("install");

    let mut ch = cmd.spawn()?;
    if !ch.wait()?.success() {
        anyhow::bail!("failed to build python");
    }

    Ok(())
}
