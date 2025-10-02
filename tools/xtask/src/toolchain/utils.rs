use std::{
    fs::{self, read_dir, remove_dir_all, File},
    io::Write,
    path::PathBuf,
    process::Command,
};

use anyhow::{anyhow, Context};
use git2::Repository;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;

use super::{get_bin_path, get_toolchain_path, BootstrapOptions};

const BASE_REPO_URL: &str = "https://github.com/twizzler-operating-system/twizzler";
// const BASE_REPO_URL: &str = "https://github.com/suri-codes/twizzler";

pub async fn download_file(client: &Client, url: &str, path: &str) -> anyhow::Result<()> {
    use futures_util::StreamExt;
    let res = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to download {}", url))?;

    if !res.status().is_success() {
        anyhow::bail!("HTTP error {}: {}", res.status(), url);
    }

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

pub fn install_build_tools(_cli: &BootstrapOptions) -> anyhow::Result<()> {
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
        .arg("--target")
        .arg("toolchain/install/python")
        .arg("--force-reinstall")
        .arg("--ignore-installed")
        .arg("--no-warn-script-location")
        .arg("--upgrade")
        .arg("meson")
        .arg("ninja")
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to install meson and ninja");
    }

    Ok(())
}

/// Removes binaries from the `/install` directory during bootstrap
pub fn prune_bins() -> anyhow::Result<()> {
    let wanted_bins = [
        "bindgen",
        "clang",
        "clang++",
        "rust-gdb",
        "rust-gdbgui",
        "rust-lldb",
        "rustc",
        "rustdoc",
        "set-xcode-analyzer",
    ];

    let bin_path = get_bin_path()?;
    for entry in fs::read_dir(&bin_path)? {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str() {
            if !wanted_bins.contains(&name) {
                let mut unwanted_bin = bin_path.clone();
                unwanted_bin.push(name);
                // we delete
                Command::new("rm").arg(unwanted_bin).status()?;
            }
        }
    }
    Ok(())
}

/// Removes everything needed to build a toolchain, i.e. rust repo, mlibc repo, etc.
pub fn prune_toolchain() -> anyhow::Result<()> {
    let submodule_deinit = |path: &PathBuf| -> anyhow::Result<()> {
        Command::new("git")
            .arg("submodule")
            .arg("deinit")
            .arg("-f")
            .arg(path)
            .status()?;

        Ok(())
    };

    let rust = PathBuf::from("toolchain/src/rust");
    let mlibc = PathBuf::from("toolchain/src/mlibc");

    submodule_deinit(&rust)?;
    submodule_deinit(&mlibc)?;

    remove_dir_all("toolchain/install")?;

    Ok(())
}

/// generates generic tag using currently checked out submodules
/// only used to name a github release
/// format: toolchain_<hash>.tar.zst
pub fn generate_tag() -> anyhow::Result<String> {
    let hash = generate_hash()?;

    let tag = format!("toolchain_{hash}");

    Ok(tag)
}

/// generates tag using currently checked out submodules, as well as
/// host operating system/architecture
/// format: toolchain_<x86|aarch64>_<linux|darwin>_<hash>.tar.zst
pub fn generate_os_arch_tag() -> anyhow::Result<String> {
    let hash = generate_hash()?;

    let arch = {
        let output = Command::new("uname").arg("-m").output()?;
        let stdout = String::from_utf8(output.stdout)?;
        stdout.trim().to_owned()
    };

    let os = {
        let output = Command::new("uname").output()?;
        let stdout = String::from_utf8(output.stdout)?;
        stdout.trim().to_owned()
    };

    let tag = format!("toolchain_{arch}_{os}_{hash}");

    Ok(tag)
}

/// Generates toolchain hash in the following format: <rust>_<mlibc>_<abi>.
fn generate_hash() -> anyhow::Result<String> {
    let repo = Repository::open("./")?;

    let submodules = repo.submodules()?;

    let get_head = |submodule_path: &str| -> String {
        let oid = submodules
            .iter()
            .find(|e| e.name().expect("submodulue should have a name") == submodule_path)
            .unwrap_or_else(|| panic!("submodule not found at path: {}", submodule_path))
            .head_id()
            .expect("head should exist")
            .to_string();

        // truncate the oid to 7 characters, if its good enough for github, its good enough for us
        let (head, _) = oid.split_at(7);
        head.to_owned()
    };

    let rust_head = get_head("toolchain/src/rust");
    let mlibc_head = get_head("toolchain/src/mlibc");
    let abi_head = get_head("src/abi");

    Ok(format!("{rust_head}-{mlibc_head}-{abi_head}"))
}

/// Compresses the active toolchain
pub fn compress_toolchain() -> anyhow::Result<()> {
    let tag = generate_os_arch_tag()?;

    let tc_path = get_toolchain_path()?;

    let _ = Command::new("tar")
        .arg("--zstd")
        .arg("-c")
        .arg("-f")
        .arg([tag.as_str(), ".tar.zst"].concat())
        .arg(tc_path)
        .spawn()?;

    Ok(())
}

/// Returns the download url for the appropriate toolchain,
/// as well as checking if the toolchain exists remotely
pub async fn get_checked_download_url() -> anyhow::Result<String> {
    let remote_tc_url = {
        let toolchain_tag = generate_tag()?;
        let os_arch_tag = generate_os_arch_tag()?;
        let archive_filename = format!("{}.tar.zst", os_arch_tag);
        format!(
            "{}/releases/download/{}/{}",
            BASE_REPO_URL, toolchain_tag, archive_filename
        )
    };

    let client = Client::new();

    client
        .get(&remote_tc_url)
        .send()
        .await?
        .error_for_status()
        .map(|_| Ok(remote_tc_url))?
}

/// Pulls down the toolchain, erroring if toolchain doesnt exist remotely
pub async fn pull_toolchain() -> anyhow::Result<()> {
    let tc_tag = generate_tag()?;

    let download_url = get_checked_download_url().await.map_err(|e| {
        let error_msg = e.to_string();
        if error_msg.contains("404") {
            return anyhow!(
                r#"
Toolchain release not found, it might not exist for the tag {tc_tag}
or it hasn't been built for your os-architecture combination!
You can check at {BASE_REPO_URL}/releases

If you would like a toolchain to be built for your
platfrom please create a github issue with your os and architecture.

If you are comfortable with compiling the toolchain locally please run

git submodule update --init --recursive
cargo toolchain bootstrap
                    "#,
            );
        }
        e
    })?;

    let tc_os_arch_tag = generate_os_arch_tag()?;
    let archive_filename = format!("{}.tar.zst", tc_os_arch_tag);
    println!("pulling toolchain for {}", tc_tag);

    let client = Client::new();

    let local_archive_path = archive_filename;

    download_file(&client, &download_url, &local_archive_path).await?;

    println!("download suceeeded!");

    println!("extracting toolchain");
    decompress_toolchain(PathBuf::from(&local_archive_path))?;

    println!("cleaning up archive file");
    fs::remove_file(&local_archive_path)?;

    println!("extraction complete");

    Ok(())
}

/// Decompresses the toolchain archive and places it inside `/toolchain`
pub fn decompress_toolchain(archive_path: PathBuf) -> anyhow::Result<()> {
    // `tar --zstd -xf toolchain_arm64_Darwin_46042ba-1a94b71-4543a3e.tar.zst --strip-components=1
    // -C toolchain/`
    let _ = Command::new("tar")
        .arg("--zstd")
        .arg("-xf")
        .arg(&archive_path)
        .arg("--strip-components=1")
        .arg("-C")
        .arg("toolchain/")
        .status()?;

    Ok(())
}

pub fn get_installed_toolchains() -> anyhow::Result<Vec<String>> {
    let mut toolchains = Vec::new();
    for entry in read_dir("toolchain/")? {
        // we dont want to terminate on error
        if entry.is_err() {
            continue;
        }

        let dir_name = entry.unwrap().file_name().into_string().unwrap();
        if dir_name.starts_with("toolchain") {
            toolchains.push(dir_name);
        }
    }

    Ok(toolchains)
}
