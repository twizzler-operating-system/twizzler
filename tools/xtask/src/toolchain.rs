use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    vec,
};

use anyhow::Context;
use guess_host_triple::guess_host_triple;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use toml_edit::Document;

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

pub(crate) fn do_bootstrap(cli: BootstrapOptions) -> anyhow::Result<()> {
    if !cli.skip_submodules {
        /*
        let status = Command::new("git")
            .arg("submodule")
            .arg("update")
            .arg("--init")
            .arg("--recursive")
            .arg("--depth=1")
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to update git submodules");
        }
        */
        fs_extra::dir::create_all("toolchain/install", false)?;
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_files(&client))?;
    }

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

    let path = std::env::var("PATH").unwrap();
    let lld_bin = get_lld_bin(guess_host_triple().unwrap())?;
    std::env::set_var("PATH", format!("{}:{}", lld_bin.to_string_lossy(), path));

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

    for target in &crate::triple::all_possible_platforms() {
        build_crtx("crti", target)?;
        build_crtx("crtn", target)?;
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

    Ok(())
}

pub fn set_dynamic() {
    std::env::set_var(
        "RUSTFLAGS",
        "-C prefer-dynamic=y -Z staticlib-prefer-dynamic=y",
    );
}

pub fn set_static() {
    std::env::set_var(
        "RUSTFLAGS",
        "-C prefer-dynamic=n -Z staticlib-prefer-dynamic=n -C target-feature=+crt-static -C relocation-model=static",
    );
}

pub fn set_cc() {
    // When compiling crates that compile C code (e.g. alloca), we need to use our clang.
    let clang_path = Path::new("toolchain/src/rust/build/host/llvm/bin/clang")
        .canonicalize()
        .unwrap();
    std::env::set_var("CC", clang_path);

    // We don't have any real system-include files, but we can provide these extremely simple ones.
    let inc_path = Path::new("toolchain/src/bootstrap-include")
        .canonicalize()
        .unwrap();
    // We don't yet support stack protector. Also, don't pull in standard lib includes, as those may
    // go to the system includes.
    let cflags = format!("-fno-stack-protector -nostdlibinc -I{}", inc_path.display());
    std::env::set_var("CFLAGS", cflags);
}

pub fn clear_cc() {
    std::env::remove_var("CC");
    std::env::remove_var("CFLAGS");
}

pub fn clear_rustflags() {
    std::env::remove_var("RUSTFLAGS");
}

pub(crate) fn init_for_build(abi_changes_ok: bool) -> anyhow::Result<()> {
    if needs_reinstall()? && !abi_changes_ok {
        eprintln!("!! You'll need to recompile your toolchain. Running `cargo bootstrap` should resolve the issue.");
        anyhow::bail!("toolchain needs reinstall: run cargo bootstrap.");
    }
    std::env::set_var("RUSTC", &get_rustc_path()?);
    std::env::set_var("RUSTDOC", &get_rustdoc_path()?);

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
            "{}:{}:{}:{}",
            rustlib_bin.to_string_lossy(),
            lld_bin.to_string_lossy(),
            llvm_bin.to_string_lossy(),
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

fn generate_config_toml() -> anyhow::Result<()> {
    /* We need to add two(ish) things to the config.toml for rustc: the paths of tools for each twizzler target (built by LLVM as part
    of rustc), and the host triple (added to the list of triples to support). */
    let mut data = File::open("toolchain/src/config.toml")?;
    let mut buf = String::new();
    data.read_to_string(&mut buf)?;
    let commented =
        String::from("# This file was auto-generated by xtask. Do not edit directly.\n") + &buf;
    let mut toml = commented.parse::<Document>()?;
    let host_triple = guess_host_triple().unwrap();

    let llvm_bin = get_llvm_bin(host_triple)?;
    toml["build"]["target"]
        .as_array_mut()
        .unwrap()
        .push(host_triple);

    toml["target"][host_triple]["llvm-has-rust-patches"] = toml_edit::value(true);
    toml["target"][host_triple]["cc"] = toml_edit::value("/usr/bin/clang");
    toml["target"][host_triple]["cxx"] = toml_edit::value("/usr/bin/clang++");
    toml["target"][host_triple]["linker"] = toml_edit::value("/usr/bin/clang++");

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

    let mut out = File::create("toolchain/src/rust/config.toml")?;
    out.write_all(toml.to_string().as_bytes())?;
    Ok(())
}
