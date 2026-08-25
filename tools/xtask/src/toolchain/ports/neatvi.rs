use std::{path::Path, process::Command, thread::available_parallelism};

use reqwest::Client;

use crate::{toolchain::download_file, triple::Triple};

pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building neatvi for {}", triple);

    let sysroot_dir = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .canonicalize()?;

    let url = "https://github.com/aligrudi/neatvi/archive/refs/tags/20.tar.gz";

    let cont_dir = Path::new("toolchain/install/build/ports/neatvi");
    std::fs::create_dir_all(&cont_dir)?;
    let tar_file = cont_dir.join("neatvi-20.tar.gz");
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
        .arg("neatvi-20.tar.gz")
        .current_dir("toolchain/install/build/ports/neatvi")
        .status()?;

    if !status.success() {
        anyhow::bail!("failed to extract neatvi");
    }

    // No configure and no out-of-tree build support; build in the extracted source dir.
    let src_dir = Path::new("toolchain/install/build/ports/neatvi/neatvi-20").canonicalize()?;
    let bin_dir = Path::new("toolchain/install/bin").canonicalize()?;

    let cc = format!(
        "{} -target {} --sysroot {}",
        bin_dir.join("clang").display(),
        triple,
        sysroot_dir.display()
    );

    let mut cmd = Command::new("make");
    cmd.current_dir(&src_dir)
        .arg(format!("CC={}", cc))
        .arg("-j")
        .arg(available_parallelism().unwrap().get().to_string());

    let status = cmd.status()?;
    if !status.success() {
        anyhow::bail!("failed to build neatvi");
    }

    let install_bin = sysroot_dir.join("pkg/neatvi/bin");
    std::fs::create_dir_all(&install_bin)?;
    for prog in ["vi", "stag"] {
        std::fs::copy(src_dir.join(prog), install_bin.join(prog))?;
    }

    Ok(())
}
