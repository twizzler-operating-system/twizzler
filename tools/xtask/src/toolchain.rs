use std::{fs::File, io::Write, path::Path, process::Command};

use anyhow::Context;
use fs_extra::dir::CopyOptions;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;

use crate::{triple::Triple, BootstrapOptions};

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
    let pb = ProgressBar::new(total_size);
    pb.set_style(ProgressStyle::default_bar().template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})").progress_chars("#>-"));

    let msg = format!("Downloading {}", url);
    pb.set_message(&msg);

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
    pb.finish_with_message(&format!("downloaded {} => {}", url, path));
    Ok(())
}

fn create_stamp() {
    let mut file =
        std::fs::File::create("toolchain/install/stamp").expect("failed to create stamp file");
    file.write_all(&[0]).expect("failed to write stamp file");
}

pub fn needs_reinstall() -> bool {
    let stamp = std::fs::metadata("toolchain/install/stamp");
    if stamp.is_err() {
        return true;
    }
    let stamp = stamp
        .unwrap()
        .modified()
        .expect("failed to get system time from metadata");
    for entry in walkdir::WalkDir::new("src/lib/twizzler-abi").min_depth(1) {
        let entry = entry.expect("error walking directory");
        let stat = entry.metadata().expect(&format!(
            "failed to read metadata for {}",
            entry.path().display()
        ));
        let mtime = stat
            .modified()
            .expect("failed to get system time from mtime");

        if mtime >= stamp {
            return true;
        }
    }
    false
}

fn build_crtx(name: &str, build_info: &Triple) -> anyhow::Result<()> {
    let objname = format!("{}.o", name);
    let srcname = format!("{}.rs", name);
    let sourcepath = Path::new("toolchain/src/").join(srcname);
    let objpath = format!(
        "toolchain/install/lib/rustlib/{}/lib/{}",
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
    download_file(
        &client,
        "http://melete.soe.ucsc.edu:9000/OVMF.fd",
        "toolchain/install/OVMF.fd",
    )
    .await?;
    download_file(
        &client,
        "http://melete.soe.ucsc.edu:9000/BOOTX64.EFI",
        "toolchain/install/BOOTX64.EFI",
    )
    .await?;
    Ok(())
}

pub(crate) fn do_bootstrap(cli: BootstrapOptions) -> anyhow::Result<()> {
    if !cli.skip_submodules {
        let status = Command::new("git")
            .arg("submodule")
            .arg("update")
            .arg("--init")
            .arg("--recursive")
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to update git submodules");
        }
        let client = Client::new();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(download_files(&client))?;
    }

    let res = std::fs::hard_link(
        "toolchain/src/config.toml",
        "toolchain/src/rust/config.toml",
    );
    if let Err(e) = res {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            anyhow::bail!("failed to create hardlink config.toml");
        }
    }
    let res = std::fs::remove_dir_all("toolchain/src/rust/library/twizzler-abi");
    if let Err(e) = res {
        if e.kind() != std::io::ErrorKind::NotFound {
            anyhow::bail!("failed to remove copied twizzler-abi");
        }
    }
    fs_extra::copy_items(
        &["src/lib/twizzler-abi"],
        "toolchain/src/rust/library/",
        &CopyOptions::new(),
    )?;

    let status = Command::new("./x.py")
        .arg("install")
        .current_dir("toolchain/src/rust")
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to compile rust toolchain");
    }

    for target in &crate::triple::all_possible_platforms() {
        build_crtx("crti", target)?;
        build_crtx("crtn", target)?;
    }

    let _stamp = create_stamp();

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

pub(crate) fn init_for_build(is_doc: bool) -> anyhow::Result<()> {
    if needs_reinstall() && !is_doc {
        anyhow::bail!("detected changes to twizzler-abi not reflected in current toolchain. This is probably because the twizzler-abi crate files were updated, so you need to run `cargo bootstrap --skip-submodules' again.");
    }
    std::env::set_var("RUSTC", &get_rustc_path()?);
    std::env::set_var("RUSTDOC", &get_rustdoc_path()?);
    Ok(())
}
