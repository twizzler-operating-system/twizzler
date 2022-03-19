use std::{io::Write, path::Path, process::Command};

use fs_extra::dir::CopyOptions;

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

pub(crate) fn init_for_build() -> anyhow::Result<()> {
    if needs_reinstall() {
        anyhow::bail!("detected changes to twizzler-abi not reflected in current toolchain. This is probably because the twizzler-abi crate files were updated, so you need to run `cargo bootstrap --skip-submodules' again.");
    }
    std::env::set_var("RUSTC", &get_rustc_path()?);
    std::env::set_var("RUSTDOC", &get_rustdoc_path()?);
    Ok(())
}
