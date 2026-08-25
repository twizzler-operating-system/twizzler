use std::{path::Path, process::Command, thread::available_parallelism};

use reqwest::Client;

use crate::{toolchain::download_file, triple::Triple};

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building libssh2 for {}", triple);

    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let url = "https://libssh2.org/download/libssh2-1.11.1.tar.xz";

    let cont_dir = Path::new("toolchain/install/build/ports/libssh2");
    std::fs::create_dir_all(&cont_dir)?;
    let tar_file = cont_dir.join("libssh2-1.11.1.tar.xz");
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
        .arg("-xJf")
        .arg("libssh2-1.11.1.tar.xz")
        .current_dir("toolchain/install/build/ports/libssh2")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract libssh2");
    }

    let src_dir =
        Path::new("toolchain/install/build/ports/libssh2/libssh2-1.11.1").canonicalize()?;
    let build_dir =
        Path::new("toolchain/install/build/ports/libssh2/build").join(triple.to_string());
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
        .arg("--prefix=/pkg/libssh2")
        .arg("--enable-shared")
        .arg("--with-crypto=openssl")
        .arg(format!(
            "--with-libssl-prefix={}",
            sysroot_dir.join("pkg/openssl").display()
        ))
        .arg("--with-libz")
        .arg(format!(
            "--with-libz-prefix={}",
            sysroot_dir.join("pkg/zlib").display()
        ))
        .arg("--disable-examples-build");
    cmd.env("DESTDIR", &install_dir);

    let cflags = format!(
        "-target {} --sysroot {} -fPIC",
        triple,
        sysroot_dir.display(),
    );
    // The --with-*-prefix flags only feed libssh2's own checks; the compiler still needs the
    // non-standard sysroot pkg paths spelled out.
    let dep_includes = format!(
        " -I{} -I{}",
        sysroot_dir.join("pkg/openssl/include").display(),
        sysroot_dir.join("pkg/zlib/include").display()
    );
    let dep_libs = format!(
        " -L{} -L{} -L{}",
        sysroot_dir.join("pkg/openssl/lib").display(),
        sysroot_dir.join("pkg/openssl/lib64").display(),
        sysroot_dir.join("pkg/zlib/lib").display()
    );

    cmd.env("PKG_CONFIG_PATH", "/bin/false");
    cmd.env("PKG_CONFIG", "/bin/false");
    cmd.env("CFLAGS", format!("{}{}", cflags, dep_includes));
    cmd.env("CXXFLAGS", format!("{}{}", cflags, dep_includes));
    cmd.env("CPPFLAGS", format!("{}{}", cflags, dep_includes));
    cmd.env("LDFLAGS", format!("{}{}", cflags, dep_libs));
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
        anyhow::bail!("failed to configure libssh2");
    }

    // libssh2's bundled libtool.m4 predates the twizzler triple and disables shared libs.
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
        anyhow::bail!("failed to build libssh2");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(&build_dir)
        .arg("install")
        .arg(format!("DESTDIR={}", sysroot_dir.display()))
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to install libssh2");
    }

    Ok(())
}
