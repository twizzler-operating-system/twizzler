use std::{path::Path, process::Command};

use crate::{toolchain::BootstrapOptions, triple::Triple};

pub fn build_rust(cli: &BootstrapOptions) -> anyhow::Result<()> {
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
    Ok(())
}

fn build_crtx(name: &str, build_info: &Triple) -> anyhow::Result<()> {
    let objname = format!("{}.o", name);
    let srcname = format!("{}.rs", name);
    let sourcepath = Path::new("toolchain/src/").join(srcname);
    let objpath = format!(
        "toolchain/install/lib/rustlib/{}/lib/self-contained/{}",
        build_info, objname
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
