use std::path::Path;

use reqwest::Client;

use crate::{toolchain::download_file, triple::Triple};

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building libgit2 for {}", triple);

    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let url = "https://github.com/libgit2/libgit2/archive/refs/tags/v1.9.7.tar.gz";

    let cont_dir = Path::new("toolchain/install/build/ports/libgit2");
    std::fs::create_dir_all(&cont_dir)?;
    let tar_file = cont_dir.join("libgit2-1.9.7.tar.gz");
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
        .arg("libgit2-1.9.7.tar.gz")
        .current_dir("toolchain/install/build/ports/libgit2")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract libgit2");
    }

    let src_dir =
        Path::new("toolchain/install/build/ports/libgit2/libgit2-1.9.7").canonicalize()?;
    let build_dir =
        Path::new("toolchain/install/build/ports/libgit2/build").join(triple.to_string());
    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;
    let install_dir = sysroot_dir.join("pkg/libgit2");

    let pkgconfig_paths = format!(
        "{}:{}:{}",
        sysroot_dir.join("pkg/libssh2/lib/pkgconfig").display(),
        sysroot_dir.join("pkg/openssl/lib/pkgconfig").display(),
        sysroot_dir.join("pkg/zlib/lib/pkgconfig").display()
    );

    let mut cfg = cmake::Config::new(&src_dir);
    cfg.out_dir(&build_dir);
    super::llvm::setup_cmake(&mut cfg, Some(&install_dir))?;
    super::llvm::setup_cmake_twizzler(&mut cfg, triple, vec!["-fPIC".to_string()])?;

    cfg.define("BUILD_SHARED_LIBS", "ON")
        .define("BUILD_TESTS", "OFF")
        .define("BUILD_CLI", "OFF")
        .define("BUILD_EXAMPLES", "OFF")
        .define("BUILD_FUZZERS", "OFF")
        .define("USE_HTTPS", "OpenSSL")
        .define("USE_SSH", "libssh2")
        // Needs iconv.h, which mlibc doesn't provide.
        .define("USE_NTLMCLIENT", "OFF")
        .define("REGEX_BACKEND", "builtin")
        .define(
            "OPENSSL_ROOT_DIR",
            sysroot_dir.join("pkg/openssl").display().to_string(),
        )
        .define(
            "ZLIB_ROOT",
            sysroot_dir.join("pkg/zlib").display().to_string(),
        );
    // The dep .pc files carry their configured /pkg/<name> prefixes; SYSROOT_DIR re-roots them.
    cfg.env("PKG_CONFIG_LIBDIR", &pkgconfig_paths)
        .env("PKG_CONFIG_SYSROOT_DIR", sysroot_dir.display().to_string());

    cfg.build();

    Ok(())
}
