use std::{
    fs::{remove_dir_all, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    vec,
};

use anyhow::Context;
use fs_extra::dir::CopyOptions;
use guess_host_triple::guess_host_triple;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use toml_edit::DocumentMut;
use walkdir::WalkDir;

use crate::{
    triple::{all_possible_platforms, Triple},
    BootstrapOptions,
};

pub fn get_toolchain_path() -> anyhow::Result<String> {
    Ok("toolchain/install".to_string())
}

pub fn get_rustc_path() -> anyhow::Result<String> {
    let toolchain = get_toolchain_path()?;
    Ok(format!("{}/bin/rustc", toolchain))
}

pub fn get_rustdoc_path() -> anyhow::Result<String> {
    let toolchain = get_toolchain_path()?;
    Ok(format!("{}/bin/rustdoc", toolchain))
}

pub async fn download_file(client: &Client, url: &str, path: &str) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    let res = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to download {}", url))?;
    let total_size = res
        .content_length()
        .with_context(|| format!("failed to get content-length for {}", url))?;
    println!("downloading {}", url);
    let pb = ProgressBar::new(total_size);
    pb.set_style(ProgressStyle::default_bar().template("{prefix}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?.progress_chars("#>-"));

    let mut file = File::create(path).with_context(|| format!("failed to create file {}", path))?;
    let mut downloaded: u64 = 0;
    let mut stream = res.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item.with_context(|| format!("error while downloading file {}", url))?;
        file.write_all(&chunk)
            .with_context(|| format!("error while writing to file {}", path))?;
        let new = std::cmp::min(downloaded + (chunk.len() as u64), total_size);
        downloaded = new;
        pb.set_position(new);
    }
    pb.finish_and_clear();
    println!("downloaded {} => {}", url, path);
    Ok(())
}

fn get_rust_commit() -> anyhow::Result<String> {
    let repo = git2::Repository::open("toolchain/src/rust")?;
    let cid = repo.head()?.peel_to_commit()?.id();
    Ok(cid.to_string())
}

fn get_abi_version() -> anyhow::Result<semver::Version> {
    let toml = cargo_toml::Manifest::from_path("src/abi/rt-abi/Cargo.toml")?;
    let abipkg = toml.package.as_ref().unwrap();
    Ok(semver::Version::parse(&abipkg.version.get().unwrap())?)
}

static STAMP_PATH: &str = "toolchain/install/stamp";
static NEXT_STAMP_PATH: &str = "toolchain/install/.next_stamp";
fn write_stamp(path: &str, rust_cid: &String, abi_vers: &String) -> anyhow::Result<()> {
    std::fs::write(path, &format!("{}/{}", rust_cid, abi_vers))?;
    Ok(())
}

fn move_stamp() -> anyhow::Result<()> {
    std::fs::rename(NEXT_STAMP_PATH, STAMP_PATH)?;
    Ok(())
}

pub fn needs_reinstall() -> anyhow::Result<bool> {
    let stamp = std::fs::metadata(STAMP_PATH);
    if stamp.is_err() {
        return Ok(true);
    }
    let stamp_data = std::fs::read_to_string(STAMP_PATH)?;
    let vers = stamp_data.split("/").collect::<Vec<_>>();
    if vers.len() != 2 {
        eprintln!("WARNING -- stamp file has invalid format.");
        return Ok(true);
    }

    let rust_commit = get_rust_commit()?;
    let abi_version = get_abi_version()?;
    // TODO: in the future, we'll want to do a full ABI semver req check here. For now
    // we'll just do simple equality checking, especially during development when the
    // ABI may be changing often anyway, and is pre-1.0.
    if vers[0] != rust_commit || vers[1] != abi_version.to_string() {
        eprintln!("WARNING -- Your toolchain is out of date. This is probably because");
        eprintln!("        -- the repository updated to a new rustc commit, or the ABI");
        eprintln!("        -- files were updated.");
        eprintln!("Installed rust toolchain commit: {}", vers[0]);
        eprintln!("toolchain/src/rust: HEAD commit: {}", rust_commit);
        eprintln!("Installed toolchain has ABI version: {}", vers[1]);
        eprintln!("src/abi/rt-abi provides ABI version: {}", abi_version);
        eprintln!("note -- currently the ABI version check requires exact match, not semver.");
        return Ok(true);
    }

    Ok(false)
}

fn build_crtx(name: &str, build_info: &Triple) -> anyhow::Result<()> {
    let objname = format!("{}.o", name);
    let srcname = format!("{}.rs", name);
    let sourcepath = Path::new("toolchain/src/").join(srcname);
    let objpath = format!(
        "toolchain/install/lib/rustlib/{}/lib/self-contained/{}",
        build_info.to_string(),
        objname
    );
    let objpath = Path::new(&objpath);
    println!("building {:?} => {:?}", sourcepath, objpath);
    let status = Command::new("toolchain/install/bin/rustc")
        .arg("--emit")
        .arg("obj")
        .arg("-o")
        .arg(objpath)
        .arg(sourcepath)
        .arg("--crate-type")
        .arg("staticlib")
        .arg("-C")
        .arg("panic=abort")
        .arg("--target")
        .arg(build_info.to_string())
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to compile {}::{}", name, build_info.to_string());
    }

    Ok(())
}

async fn download_files(client: &Client) -> anyhow::Result<()> {
    // efi binaries for x86 machines
    download_file(
        client,
        "http://twizzler.io/dist/bootfiles/OVMF.fd",
        "toolchain/install/OVMF.fd",
    )
    .await?;
    download_file(
        client,
        "http://twizzler.io/dist/bootfiles/BOOTX64.EFI",
        "toolchain/install/BOOTX64.EFI",
    )
    .await?;
    // efi binaries for aarch64 machines
    download_file(
        client,
        "http://twizzler.io/dist/bootfiles/QEMU_EFI.fd",
        "toolchain/install/OVMF-AA64.fd",
    )
    .await?;
    download_file(
        client,
        "http://twizzler.io/dist/bootfiles/BOOTAA64.EFI",
        "toolchain/install/BOOTAA64.EFI",
    )
    .await?;

    Ok(())
}

fn install_build_tools(_cli: &BootstrapOptions) -> anyhow::Result<()> {
    println!("installing bindgen");
    let status = Command::new("cargo")
        .arg("install")
        .arg("--root")
        .arg("toolchain/install")
        .arg("bindgen-cli")
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to install bindgen");
    }

    println!("installing meson & ninja");
    let status = Command::new("pip3")
        .arg("install")
        .arg("--prefix")
        .arg("toolchain/install")
        .arg("--force-reinstall")
        .arg("--ignore-installed")
        .arg("--no-warn-script-location")
        .arg("meson")
        .arg("ninja")
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to install meson and ninja");
    }

    let mut target = None;
    let mut already_done = false;
    for file in WalkDir::new("toolchain/install/lib").max_depth(1) {
        if let Ok(file) = file {
            let name = file.file_name().to_str().unwrap();
            if name.starts_with("python") && name != "python" {
                target = Some(name.to_string());
            }
            if name == "python" {
                already_done = true;
            }
        }
    }

    if !already_done {
        if let Some(target) = target {
            std::os::unix::fs::symlink(target, "toolchain/install/lib/python")?;
        }
    }

    Ok(())
}

pub(crate) fn do_bootstrap(cli: BootstrapOptions) -> anyhow::Result<()> {
    fs_extra::dir::create_all("toolchain/install", false)?;
    if !cli.skip_downloads {
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_files(&client))?;
    }

    install_build_tools(&cli)?;
    let current_dir = std::env::current_dir().unwrap();
    std::env::set_var(
        "PYTHONPATH",
        current_dir.join("toolchain/install/lib/python/site-packages"),
    );

    let _ = std::fs::remove_file("toolchain/src/rust/config.toml");
    generate_config_toml()?;

    let _ = fs_extra::dir::remove("toolchain/src/rust/library/twizzler-abis");
    let status = Command::new("cp")
        .arg("-R")
        .arg("src/abi")
        .arg("toolchain/src/rust/library/twizzler-abis")
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to copy twizzler ABI files");
    }

    println!("copying headers");
    let status = Command::new("cp")
        .arg("-R")
        .arg("src/abi/include")
        .arg("toolchain/src/mlibc/sysdeps/twizzler")
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to copy twizzler ABI headers");
    }

    let path = std::env::var("PATH").unwrap();
    let lld_bin = get_lld_bin(guess_host_triple().unwrap())?;
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}:{}",
            lld_bin.to_string_lossy(),
            std::fs::canonicalize("toolchain/install/bin")
                .unwrap()
                .to_string_lossy(),
            path
        ),
    );

    for target_triple in all_possible_platforms() {
        let current_dir = std::env::current_dir().unwrap();
        let sysroot_dir = current_dir.join(format!(
            "toolchain/install/sysroots/{}",
            target_triple.to_string()
        ));
        let build_dir_name = format!("build-{}", target_triple.to_string());
        let src_dir = current_dir.join("toolchain/src/mlibc");
        let build_dir = src_dir.join(&build_dir_name);
        let cross_file = format!("{}/meson-cross-twizzler.txt", sysroot_dir.display());

        std::fs::create_dir_all(&sysroot_dir)?;

        let mut cf = File::create(&cross_file)?;

        writeln!(&mut cf, "[binaries]")?;
        for tool in [
            ("c", "clang"),
            ("cpp", "clang++"),
            ("ar", "llvm-ar"),
            ("strip", "llvm-strip"),
        ] {
            let path = current_dir.join("toolchain/install/bin");
            let path = path.join(tool.1);
            writeln!(&mut cf, "{} = '{}'", tool.0, path.display())?;
        }

        writeln!(&mut cf, "[built-in options]")?;
        let lld_path = current_dir.join("toolchain/src/rust/build/host/lld/bin");
        for tool in ["c_args", "c_link_args", "cpp_args", "cpp_link_args"] {
            writeln!(
                &mut cf,
                "{} = ['-B{}', '-isysroot', '{}', '--sysroot', '{}', '-target', '{}']",
                tool,
                lld_path.display(),
                sysroot_dir.display(),
                sysroot_dir.display(),
                target_triple.to_string()
            )?;
        }

        writeln!(&mut cf, "[host_machine]")?;
        writeln!(&mut cf, "system = 'twizzler'")?;
        writeln!(&mut cf, "cpu_family = '{}'", target_triple.arch.to_string())?;
        writeln!(&mut cf, "cpu = '{}'", target_triple.arch.to_string())?;
        writeln!(&mut cf, "endian = 'little'")?;
        drop(cf);

        let _ = remove_dir_all(&build_dir);
        let status = Command::new("meson")
            .arg("setup")
            .arg(format!("-Dprefix={}", sysroot_dir.display()))
            .arg("-Dheaders_only=true")
            .arg("-Ddefault_library=static")
            .arg(format!("--cross-file={}", &cross_file))
            .arg("--buildtype=debugoptimized")
            .arg(&build_dir)
            .current_dir(current_dir.join("toolchain/src/mlibc"))
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to setup mlibc (headers only)");
        }

        let status = Command::new("meson")
            .arg("install")
            .arg("-q")
            .arg("-C")
            .arg(&build_dir)
            .current_dir(current_dir.join("toolchain/src/mlibc"))
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to install mlibc headers");
        }
    }
    let current_dir = std::env::current_dir().unwrap();
    let builtin_headers =
        current_dir.join("toolchain/src/rust/build/host/llvm/lib/clang/20/include/");
    std::env::set_var("TWIZZLER_ABI_BUILTIN_HEADERS", builtin_headers);

    let keep_args = if cli.keep_early_stages {
        vec![
            "--keep-stage",
            "0",
            "--keep-stage-std",
            "0",
            "--keep-stage",
            "1",
            "--keep-stage-std",
            "1",
        ]
    } else {
        vec![]
    };

    std::env::set_var("BOOTSTRAP_SKIP_TARGET_SANITY", "1");

    if !cli.skip_rust {
        let status = Command::new("./x.py")
            .arg("install")
            .args(&keep_args)
            .current_dir("toolchain/src/rust")
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to compile rust toolchain");
        }

        let src_status = Command::new("./x.py")
            .arg("install")
            .arg("src")
            .args(keep_args)
            .current_dir("toolchain/src/rust")
            .status()?;
        if !src_status.success() {
            anyhow::bail!("failed to install rust source");
        }
    }

    for target in &crate::triple::all_possible_platforms() {
        build_crtx("crti", target)?;
        build_crtx("crtn", target)?;
        let target = target.to_string();
        println!(
            "Copy: {} -> {}",
            get_llvm_native_runtime(&target)?.display(),
            get_llvm_native_runtime_install(&target)?.display()
        );

        let _ =
            std::fs::create_dir_all(get_llvm_native_runtime_install(&target)?.parent().unwrap());

        std::fs::copy(
            get_llvm_native_runtime(&target)?,
            get_llvm_native_runtime_install(&target)?,
        )?;

        for name in &["crtbegin", "crtend", "crtbeginS", "crtendS"] {
            let src = format!("toolchain/src/rust/build/{}/native/crt/{}.o", &target, name);
            let dst = format!("toolchain/install/lib/clang/20/lib/{}/{}.o", &target, name);
            std::fs::copy(src, dst)?;
        }
        for name in &["crti", "crtn"] {
            let src = format!(
                "toolchain/install/lib/rustlib/{}/lib/self-contained/{}.o",
                &target, name
            );
            let dst = format!("toolchain/install/lib/clang/20/lib/{}/{}.o", &target, name);
            println!("Copy: {} -> {}", src, dst);
            std::fs::copy(src, dst)?;
        }
        let src = format!("toolchain/install/lib/rustlib/{}/lib/libunwind.a", &target);
        let dst = format!("toolchain/install/lib/clang/20/lib/{}/libunwind.a", &target);
        println!("Copy: {} -> {}", src, dst);
        std::fs::copy(src, dst)?;
    }
    let items = ["bin", "include", "lib", "libexec", "share"]
        .into_iter()
        .map(|name| format!("toolchain/src/rust/build/host/llvm/{}", name))
        .collect::<Vec<_>>();

    println!("copying LLVM toolchain...");
    fs_extra::copy_items(
        &items,
        "toolchain/install",
        &CopyOptions::new().overwrite(true),
    )?;

    let usr_link = "toolchain/install/usr";
    let _ = std::fs::remove_file(usr_link);
    std::os::unix::fs::symlink(".", usr_link)?;

    for target_triple in all_possible_platforms() {
        let current_dir = std::env::current_dir().unwrap();
        let sysroot_dir = current_dir.join(format!(
            "toolchain/install/sysroots/{}",
            target_triple.to_string()
        ));
        let build_dir_name = format!("build-{}", target_triple.to_string());
        let src_dir = current_dir.join("toolchain/src/mlibc");
        let build_dir = src_dir.join(&build_dir_name);
        let cross_file = format!("{}/meson-cross-twizzler.txt", sysroot_dir.display());

        let _ = remove_dir_all(&build_dir);

        let status = Command::new("meson")
            .arg("setup")
            .arg(format!("-Dprefix={}", sysroot_dir.display()))
            .arg("-Ddefault_library=static")
            .arg(format!("--cross-file={}", cross_file))
            .arg("--buildtype=debugoptimized")
            .arg(&build_dir_name)
            .current_dir(current_dir.join("toolchain/src/mlibc"))
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to setup mlibc");
        }
        let status = Command::new("meson")
            .arg("compile")
            .arg("-C")
            .arg(&build_dir_name)
            .current_dir(current_dir.join("toolchain/src/mlibc"))
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to build mlibc");
        }

        let status = Command::new("meson")
            .arg("install")
            .arg("-q")
            .arg("-C")
            .arg(&build_dir_name)
            .current_dir(current_dir.join("toolchain/src/mlibc"))
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to install mlibc");
        }

        let usr_link = sysroot_dir.join("usr");
        let _ = std::fs::remove_file(&usr_link);
        std::os::unix::fs::symlink(".", usr_link)?;
    }

    let rust_commit = get_rust_commit()?;
    let abi_version = get_abi_version()?;
    write_stamp(NEXT_STAMP_PATH, &rust_commit, &abi_version.to_string())?;
    move_stamp()?;

    if !cli.keep_old_artifacts {
        let res = std::fs::remove_dir_all("target");
        if let Err(e) = res {
            if e.kind() != std::io::ErrorKind::NotFound {
                anyhow::bail!("failed to remove old build artifacts");
            }
        }
    }

    println!("ready!");
    Ok(())
}

pub fn set_dynamic(target: &Triple) {
    // This is a bit of a cursed linker line, but it's needed to work around some limitations in
    // rust's linkage support.
    let sysroot_path = Path::new(&format!(
        "toolchain/install/sysroots/{}/lib",
        target.to_string()
    ))
    .canonicalize()
    .unwrap();
    let args = format!("-C prefer-dynamic=y -Z staticlib-prefer-dynamic=y -C link-arg=--allow-shlib-undefined -C link-arg=--undefined-glob=__TWIZZLER_SECURE_GATE_* -C link-arg=--export-dynamic-symbol=__TWIZZLER_SECURE_GATE_* -C link-arg=--warn-unresolved-symbols -Z pre-link-arg=-L -Z pre-link-arg={} -L {}", sysroot_path.display(), sysroot_path.display());
    std::env::set_var("RUSTFLAGS", args);
    std::env::set_var("CARGO_TARGET_DIR", "target/dynamic");
}

pub fn set_static() {
    std::env::set_var(
        "RUSTFLAGS",
        "-C prefer-dynamic=n -Z staticlib-prefer-dynamic=n -C target-feature=+crt-static -C relocation-model=static",
    );
    std::env::set_var("CARGO_TARGET_DIR", "target/static");
}

pub fn set_cc(target: &Triple) {
    // When compiling crates that compile C code (e.g. alloca), we need to use our clang.
    let clang_path = Path::new("toolchain/install/bin/clang")
        .canonicalize()
        .unwrap();
    std::env::set_var("CC", &clang_path);
    std::env::set_var("LD", &clang_path);
    std::env::set_var("CXX", &clang_path);

    // We don't have any real system-include files, but we can provide these extremely simple ones.
    let sysroot_path = Path::new(&format!(
        "toolchain/install/sysroots/{}",
        target.to_string()
    ))
    .canonicalize()
    .unwrap();
    // We don't yet support stack protector. Also, don't pull in standard lib includes, as those may
    // go to the system includes.
    let cflags = format!(
        "-fno-stack-protector -isysroot {} -target {} --sysroot {}",
        sysroot_path.display(),
        target.to_string(),
        sysroot_path.display(),
    );
    std::env::set_var("CFLAGS", &cflags);
    std::env::set_var("LDFLAGS", &cflags);
    std::env::set_var("CXXFLAGS", &cflags);
}

pub fn clear_cc() {
    std::env::remove_var("CC");
    std::env::remove_var("CXX");
    std::env::remove_var("LD");
    std::env::remove_var("CC");
    std::env::remove_var("CXXFLAGS");
    std::env::remove_var("CFLAGS");
    std::env::remove_var("LDFLAGS");
}

pub fn clear_rustflags() {
    std::env::remove_var("RUSTFLAGS");
    std::env::remove_var("CARGO_TARGET_DIR");
}

pub(crate) fn init_for_build(abi_changes_ok: bool) -> anyhow::Result<()> {
    if needs_reinstall()? && !abi_changes_ok {
        eprintln!("!! You'll need to recompile your toolchain. Running `cargo bootstrap` should resolve the issue.");
        anyhow::bail!("toolchain needs reinstall: run cargo bootstrap.");
    }
    std::env::set_var("RUSTC", &get_rustc_path()?);
    std::env::set_var("RUSTDOC", &get_rustdoc_path()?);
    std::env::set_var("CARGO_CACHE_RUSTC_INFO", "0");
    let current_dir = std::env::current_dir().unwrap();
    std::env::set_var(
        "PYTHONPATH",
        current_dir.join("toolchain/install/lib/python/site-packages"),
    );

    let compiler_rt_path = "toolchain/src/rust/src/llvm-project/compiler-rt";
    std::env::set_var(
        "RUST_COMPILER_RT_ROOT",
        Path::new(compiler_rt_path).canonicalize().unwrap(),
    );

    let path = std::env::var("PATH").unwrap();
    let lld_bin = get_lld_bin(guess_host_triple().unwrap())?;
    let llvm_bin = get_llvm_bin(guess_host_triple().unwrap())?;
    let rustlib_bin = get_rustlib_bin(guess_host_triple().unwrap())?;
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}:{}:{}:{}",
            rustlib_bin.to_string_lossy(),
            lld_bin.to_string_lossy(),
            llvm_bin.to_string_lossy(),
            std::fs::canonicalize("toolchain/install/bin")
                .unwrap()
                .to_string_lossy(),
            path
        ),
    );
    Ok(())
}

fn get_lld_bin(host_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let llvm_bin = curdir
        .join("toolchain/src/rust/build")
        .join(host_triple)
        .join("lld/bin");
    Ok(llvm_bin)
}

fn get_llvm_bin(host_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let llvm_bin = curdir
        .join("toolchain/src/rust/build")
        .join(host_triple)
        .join("llvm/bin");
    Ok(llvm_bin)
}

fn get_rustlib_bin(host_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let rustlib_bin = curdir
        .join("toolchain/install/lib/rustlib")
        .join(host_triple)
        .join("bin");
    Ok(rustlib_bin)
}

pub fn get_rustlib_lib(host_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let rustlib_bin = curdir
        .join("toolchain/install/lib/rustlib")
        .join(host_triple)
        .join("lib");
    Ok(rustlib_bin)
}

pub fn get_rust_lld(host_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let rustlib_bin = curdir
        .join("toolchain/src/rust/build")
        .join(host_triple)
        .join("stage1/lib/rustlib")
        .join(host_triple)
        .join("bin/rust-lld");
    Ok(rustlib_bin)
}

pub fn get_rust_stage2_std(host_triple: &str, target_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let dir = curdir
        .join("toolchain/src/rust/build")
        .join(host_triple)
        .join("stage2-std")
        .join(target_triple)
        .join("release");
    Ok(dir)
}

pub fn get_llvm_native_runtime(target_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let arch = target_triple.split("-").next().unwrap();
    let archive_name = format!("libclang_rt.builtins-{}.a", arch);
    let dir = curdir
        .join("toolchain/src/rust/build")
        .join(target_triple)
        .join("native/sanitizers/build/lib/twizzler")
        .join(archive_name);
    Ok(dir)
}

pub fn get_llvm_native_runtime_install(target_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let archive_name = "libclang_rt.builtins.a";
    let dir = curdir
        .join("toolchain/install/lib/clang/20/lib")
        .join(target_triple)
        .join(archive_name);
    Ok(dir)
}

fn generate_config_toml() -> anyhow::Result<()> {
    /* We need to add two(ish) things to the config.toml for rustc: the paths of tools for each twizzler target (built by LLVM as part
    of rustc), and the host triple (added to the list of triples to support). */
    let mut data = File::open("toolchain/src/config.toml")?;
    let mut buf = String::new();
    data.read_to_string(&mut buf)?;
    let commented =
        String::from("# This file was auto-generated by xtask. Do not edit directly.\n") + &buf;
    let mut toml = commented.parse::<DocumentMut>()?;
    let host_triple = guess_host_triple().unwrap();

    let llvm_bin = get_llvm_bin(host_triple)?;
    toml["build"]["target"]
        .as_array_mut()
        .unwrap()
        .push(host_triple);

    let host_cc = std::env::var("CC").unwrap_or("/usr/bin/clang".to_string());
    let host_cxx = std::env::var("CXX").unwrap_or("/usr/bin/clang++".to_string());
    let host_ld = std::env::var("LD").unwrap_or("/usr/bin/clang++".to_string());
    toml["target"][host_triple]["llvm-has-rust-patches"] = toml_edit::value(true);
    toml["target"][host_triple]["cc"] = toml_edit::value(host_cc);
    toml["target"][host_triple]["cxx"] = toml_edit::value(host_cxx);
    toml["target"][host_triple]["linker"] = toml_edit::value(host_ld);

    for triple in all_possible_platforms() {
        let clang = llvm_bin.join("clang").to_str().unwrap().to_string();
        // Use the C compiler as the linker.
        let linker = get_rust_lld(host_triple)?.to_str().unwrap().to_string();
        let clangxx = llvm_bin.join("clang++").to_str().unwrap().to_string();
        let ar = llvm_bin.join("llvm-ar").to_str().unwrap().to_string();

        let tstr = &triple.to_string();
        toml["target"][tstr]["cc"] = toml_edit::value(clang);
        toml["target"][tstr]["cxx"] = toml_edit::value(clangxx);
        toml["target"][tstr]["linker"] = toml_edit::value(linker);
        toml["target"][tstr]["ar"] = toml_edit::value(ar);

        toml["target"][tstr]["llvm-has-rust-patches"] = toml_edit::value(true);
        toml["target"][tstr]["llvm-libunwind"] = toml_edit::value("in-tree");

        toml["build"]["target"].as_array_mut().unwrap().push(tstr);
    }

    let mut out = File::create("toolchain/src/rust/bootstrap.toml")?;
    out.write_all(toml.to_string().as_bytes())?;
    Ok(())
}
