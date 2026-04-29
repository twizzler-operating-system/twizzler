use std::{path::Path, process::Command, thread::available_parallelism};

use crate::{
    toolchain::{bootstrap::setup_logfile, guess_host_triple},
    triple::Triple,
};

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    let src_dir = Path::new("src/ports/python/cpython").canonicalize()?;
    let build_dir = Path::new("toolchain/build/ports/cpython").join(triple.to_string());
    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;
    let bin_dir = Path::new("toolchain/install/bin").canonicalize()?;
    let install_dir = std::path::Path::new("toolchain/install/sysroots").join(&triple.to_string());
    std::fs::create_dir_all(&install_dir)?;
    let install_dir = install_dir.canonicalize()?;
    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    let log = setup_logfile("ports/cpython", "xtask-configure", Some(triple))?;
    let mut cmd = Command::new(src_dir.join("configure"));
    cmd.current_dir(&build_dir)
        .stdout(log)
        .arg("--host")
        .arg(triple.to_string())
        .arg("--target")
        .arg(triple.to_string())
        .arg("--with-mimalloc=no")
        .arg("--with-pymalloc=no")
        .arg("--prefix=/pkg/python")
        .arg("--build")
        .arg(guess_host_triple().unwrap())
        //.arg("--enable-optimizations")
        .arg("--with-build-python=python3.14")
        .arg("ac_cv_file__dev_ptmx=no")
        .arg("ac_cv_file__dev_ptc=no")
        .arg("ac_cv_func_sched_setscheduler=no")
        .arg("ac_cv_func_realpath=no")
        .arg("ac_cv_func_readlink=no")
        .arg("--enable-shared")
        .arg("--with-static-libpython=no")
        .arg("--disable-ipv6");

    let cflags = format!(
        "-target {} --sysroot {} -Og -fPIC -g -D__DEBUG__ -D__Debug__",
        triple,
        sysroot_dir.display()
    );

    cmd.env("PKG_CONFIG", "");
    cmd.env("CFLAGS", &cflags);
    cmd.env("CPPFLAGS", &cflags);
    cmd.env("CXXFLAGS", &cflags);
    cmd.env(
        "LDFLAGS",
        format!("-target {} --sysroot {}", triple, sysroot_dir.display()),
    );
    cmd.env("CC", bin_dir.join("clang").display().to_string());
    cmd.env("CPP", bin_dir.join("clang-cpp").display().to_string());
    cmd.env("CXX", bin_dir.join("clang++").display().to_string());
    cmd.env("LD", bin_dir.join("clang").display().to_string());
    cmd.env("AR", bin_dir.join("llvm-ar").display().to_string());
    cmd.env("RANLIB", bin_dir.join("llvm-ranlib").display().to_string());
    let ldshared = format!(
        "{}/clang -shared -target {} --sysroot {}",
        bin_dir.display(),
        triple,
        sysroot_dir.display()
    );
    cmd.env("BLDSHARED", &ldshared);
    cmd.env("LDSHARED", &ldshared);

    if false {
        let mut ch = cmd.spawn()?;
        if !ch.wait()?.success() {
            anyhow::bail!("failed to configure python");
        }
    }

    let log = setup_logfile("ports/cpython", "xtask-build", Some(triple))?;
    let mut cmd = Command::new("make");
    cmd.arg("-j")
        .arg(available_parallelism().unwrap().to_string());
    cmd.current_dir(&build_dir);
    cmd.stdout(log);
    cmd.env("DESTDIR", install_dir);

    cmd.arg("install");

    let mut ch = cmd.spawn()?;
    if !ch.wait()?.success() {
        anyhow::bail!("failed to build python");
    }

    Ok(())
}
