use std::{path::Path, process::Command, thread::available_parallelism};

use reqwest::Client;

use crate::{toolchain::download_file, triple::Triple};

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building curl for {}", triple);

    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let url = "https://curl.se/download/curl-8.19.0.tar.xz";

    let cont_dir = Path::new("toolchain/install/build/ports/curl");
    std::fs::create_dir_all(&cont_dir)?;
    let cont_dir = cont_dir.canonicalize()?;
    let tar_file = cont_dir.join("curl-8.19.0.tar.xz");
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
        .arg("curl-8.19.0.tar.xz")
        .current_dir("toolchain/install/build/ports/curl")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract curl");
    }

    let src_dir = Path::new("toolchain/install/build/ports/curl/curl-8.19.0").canonicalize()?;
    let build_dir = Path::new("toolchain/install/build/ports/curl/build").join(triple.to_string());
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
        .arg("--prefix=/pkg/curl")
        .arg("--enable-shared")
        // Configure resolves its Makefile conditionals before the post-configure libtool patch
        // enables shared libs, so it plans a static-linked tool and leaves the curlx_* helpers
        // out of it. Exporting everything from libcurl.so lets that tool link work anyway.
        .arg("--disable-symbol-hiding");
    // This currently is broken on macos.
    if crate::toolchain::guess_host_triple()
        .unwrap()
        .contains("darwin")
    {
        cmd.arg("--without-ssl");
    } else {
        cmd.arg(format!(
            "--with-openssl={}",
            sysroot_dir.join("pkg/openssl").display()
        ));
        cmd.arg(format!(
            "--with-libssh2={}",
            sysroot_dir.join("pkg/libssh2").display()
        ));
    }
    cmd.arg(format!(
        "--with-zlib={}",
        sysroot_dir.join("pkg/zlib").display()
    ));
    // HTTP/2. Cargo makes this mandatory: with http.multiplexing on (the default) it asks libcurl
    // for CURL_HTTP_VERSION_2_0 and treats a refusal as fatal ("failed to enable HTTP/2, is curl
    // not built right?"). configure falls back to -I/-L/-lnghttp2 from this path because
    // PKG_CONFIG is disabled below, same as the zlib/openssl/libssh2 flags above.
    cmd.arg(format!(
        "--with-nghttp2={}",
        sysroot_dir.join("pkg/nghttp2").display()
    ));
    // Runtime paths inside the guest: /pkg maps to the sysroot pkg tree on the disk image.
    cmd.arg("--with-ca-bundle=/pkg/curl/cacert.pem")
        .arg("--without-ca-path");
    // Not "--without-psl": configure ignores unknown --without-* flags and then hard-requires
    // libpsl.
    cmd.arg("--without-libpsl").arg("--enable-optimizations");
    cmd.env("DESTDIR", &install_dir);

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
        anyhow::bail!("failed to configure curl");
    }

    // curl 8.19's bundled libtool.m4 predates the twizzler triple; without this, libcurl
    // silently builds static-only.
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
        anyhow::bail!("failed to build curl");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(&build_dir)
        .arg("install")
        .arg(format!("DESTDIR={}", sysroot_dir.display()))
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to build curl");
    }

    // Matches the --with-ca-bundle=/pkg/curl/cacert.pem runtime path. Delete to refresh.
    let ca_file = sysroot_dir.join("pkg/curl/cacert.pem");
    if !std::fs::exists(&ca_file)? {
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_file(
                &client,
                "https://curl.se/ca/cacert.pem",
                ca_file.display().to_string().as_str(),
            ))?;
    }

    Ok(())
}
