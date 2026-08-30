use std::{path::Path, process::Command, thread::available_parallelism};

use reqwest::Client;

use crate::{toolchain::download_file, triple::Triple};

const VERSION: &str = "1.68.0";

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building nghttp2 for {}", triple);

    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let url = format!(
        "https://github.com/nghttp2/nghttp2/releases/download/v{}/nghttp2-{}.tar.xz",
        VERSION, VERSION
    );
    let tar_name = format!("nghttp2-{}.tar.xz", VERSION);

    let cont_dir = Path::new("toolchain/install/build/ports/nghttp2");
    std::fs::create_dir_all(&cont_dir)?;
    let tar_file = cont_dir.join(&tar_name);
    if !std::fs::exists(&tar_file)? {
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_file(
                &client,
                &url,
                tar_file.display().to_string().as_str(),
            ))?;
    }

    let status = std::process::Command::new("tar")
        .arg("-xJf")
        .arg(&tar_name)
        .current_dir(&cont_dir)
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract nghttp2");
    }

    let src_dir = cont_dir
        .join(format!("nghttp2-{}", VERSION))
        .canonicalize()?;
    let build_dir = cont_dir.join("build").join(triple.to_string());
    std::fs::create_dir_all(&build_dir)?;
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
        .arg("--prefix=/pkg/nghttp2")
        .arg("--enable-shared")
        // Only libnghttp2 is wanted (it is what gives libcurl HTTP/2). The applications and the
        // asio library pull in C++, libev, libxml2, jansson and friends, none of which are ported.
        .arg("--enable-lib-only")
        .arg("--disable-examples")
        .arg("--disable-python-bindings");

    let cflags = format!(
        "-target {} --sysroot {} -fPIC",
        triple,
        sysroot_dir.display(),
    );

    cmd.env("PKG_CONFIG_PATH", "/bin/false");
    cmd.env("PKG_CONFIG", "/bin/false");
    cmd.env("CFLAGS", &cflags);
    cmd.env("CXXFLAGS", &cflags);
    cmd.env("CPPFLAGS", &cflags);
    cmd.env("LDFLAGS", &cflags);
    cmd.env("CPP", bin_dir.join("clang-cpp").display().to_string());
    cmd.env("CC", bin_dir.join("clang").display().to_string());
    cmd.env("CXX", bin_dir.join("clang++").display().to_string());
    cmd.env("LD", bin_dir.join("clang").display().to_string());
    let mut lds = bin_dir.join("clang").display().to_string();
    lds.push_str(" -shared ");
    lds.push_str(&cflags);
    cmd.env("LDSHARED", lds);
    cmd.env("AR", bin_dir.join("llvm-ar").display().to_string());
    cmd.env("RANLIB", bin_dir.join("llvm-ranlib").display().to_string());

    let mut ch = cmd.spawn()?;
    if !ch.wait()?.success() {
        anyhow::bail!("failed to configure nghttp2");
    }

    // nghttp2's bundled libtool.m4 predates the twizzler triple and disables shared libs.
    super::patch_libtool_for_twizzler(
        &build_dir.join("libtool"),
        &bin_dir.join("clang").display().to_string(),
        triple,
        &sysroot_dir,
    )?;

    let mut cmd = Command::new("make");
    cmd.current_dir(&build_dir)
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to build nghttp2");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(&build_dir)
        .arg("install")
        .arg(format!("DESTDIR={}", sysroot_dir.display()))
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to install nghttp2");
    }

    Ok(())
}
