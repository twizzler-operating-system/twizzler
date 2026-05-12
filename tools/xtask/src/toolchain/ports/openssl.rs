use std::{
    fs::OpenOptions, io::Write, path::Path, process::Command, thread::available_parallelism,
};

use reqwest::Client;

use crate::{toolchain::download_file, triple::Triple};

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building OpenSSL for {}", triple);

    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let url =
        "https://github.com/openssl/openssl/releases/download/openssl-4.0.0/openssl-4.0.0.tar.gz";

    let cont_dir = Path::new("toolchain/install/build/ports/openssl");
    std::fs::create_dir_all(&cont_dir)?;
    let cont_dir = cont_dir.canonicalize()?;
    let tar_file = cont_dir.join("openssl-4.0.0.tar.gz");
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
        .arg("openssl-4.0.0.tar.gz")
        .current_dir("toolchain/install/build/ports/openssl")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract openssl");
    }

    let src_dir = Path::new("toolchain/install/build/ports/openssl/openssl-4.0.0");

    let bin_dir = Path::new("toolchain/install/bin").canonicalize()?;
    let cc = bin_dir.join("clang");
    let cpp = bin_dir.join("clang-cpp");
    let ar = bin_dir.join("llvm-ar");

    let x86_64_sysroot =
        Path::new("toolchain/install/sysroots/x86_64-unknown-twizzler").canonicalize()?;
    let aarch64_sysroot =
        Path::new("toolchain/install/sysroots/aarch64-unknown-twizzler").canonicalize()?;

    let config_data = format!(
        r#"
    my %targets = (
    "twizzler-common" => {{
        template         => 1,
        inherit_from     => [ "BASE_unix" ],
        CC               => "{}",
        CPP               => "{}",
        CFLAGS           => add_before(picker(default => "-Wall",
                                              debug   => "-g -O0",
                                              release => "-O2")),
        cflags           => add_before("-DL_ENDIAN",
                                       threads("-D_REENTRANT")),
        AR              => "{}",
        ARFLAGS         => "qc",
        HASHBANGPERL    => "/bin/env perl",
        sys_id           => "TWIZZLER",
        ex_libs          => "",
        perlasm_scheme   => "elf",
        thread_scheme    => "pthreads",
        dso_scheme       => "dlfcn",
        shared_target    => "gnu-shared",
        shared_cflag     => "-fPIC",
        shared_ldflag    => "-shared",
        bin_ldflags          => "-ldl",
        lib_ldflags          => "-ldl",
        shared_sonameflag=> "",
        perl_platform    => 'Unix',
        shlib_variant    => "",
        shared_extension => ".so",
    }},
    "twizzler-x86_64" => {{
        inherit_from     => [ "twizzler-common" ],
        cflags           => add("-target x86_64-unknown-twizzler --sysroot {}"),
        cppflags         => add("-target x86_64-unknown-twizzler --sysroot {}"),
        lib_cppflags         => add("-target x86_64-unknown-twizzler --sysroot {}"),
        lib_ldflags         => add("-target x86_64-unknown-twizzler --sysroot {}"),
        bin_ldflags         => add("-target x86_64-unknown-twizzler --sysroot {}"),
        bn_ops           => "SIXTY_FOUR_BIT_LONG",
    }},
    "twizzler-aarch64" => {{
        inherit_from     => [ "twizzler-common" ],
        cflags           => add("-target aarch64-unknown-twizzler --sysroot {}"),
        cppflags         => add("-target aarch64-unknown-twizzler --sysroot {}"),
        lib_cppflags         => add("-target aarch64-unknown-twizzler --sysroot {}"),
        lib_ldflags         => add("-target aarch64-unknown-twizzler --sysroot {}"),
        bin_ldflags         => add("-target aarch64-unknown-twizzler --sysroot {}"),
        bn_ops           => "SIXTY_FOUR_BIT_LONG",
    }},
);"#,
        cc.display(),
        cpp.display(),
        ar.display(),
        x86_64_sysroot.display(),
        x86_64_sysroot.display(),
        x86_64_sysroot.display(),
        x86_64_sysroot.display(),
        x86_64_sysroot.display(),
        aarch64_sysroot.display(),
        aarch64_sysroot.display(),
        aarch64_sysroot.display(),
        aarch64_sysroot.display(),
        aarch64_sysroot.display()
    );
    let config_path = cont_dir.join("openssl-4.0.0/Configurations/50-twizzler.conf");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&config_path)?;
    file.write_all(config_data.as_bytes())?;
    drop(file);

    let mut cmd = Command::new("./Configure");
    cmd.current_dir(src_dir)
        .arg("--prefix=/pkg/openssl")
        .arg("--openssldir=/pkg/openssl")
        .arg("shared")
        .arg("no-tests")
        .arg("no-apps")
        .arg(format!("twizzler-{}", triple.arch));

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to configure openssl");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(src_dir)
        .arg("SHLIB_VERSION_NUMBER=")
        .arg("SHLIB_EXT=.so")
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to build openssl");
    }

    let mut cmd = Command::new("make");
    cmd.current_dir(src_dir)
        .arg("install")
        .arg("SHLIB_VERSION_NUMBER=")
        .arg("SHLIB_EXT=.so")
        .arg(format!("DESTDIR={}", sysroot_dir.display()))
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to build openssl");
    }

    Ok(())
}
