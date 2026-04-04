use std::path::Path;

use guess_host_triple::guess_host_triple;

use super::BootstrapOptions;
use crate::{
    build::do_post_toolchain_runtime_build,
    toolchain::{compress_toolchain, prune_bins, prune_toolchain},
    triple::all_possible_platforms,
};

mod paths;
use paths::*;
mod mover;

mod install;
mod libc;
mod llvm;
mod prep;
mod rust;

pub(crate) fn do_bootstrap(cli: BootstrapOptions) -> anyhow::Result<()> {
    prep::setup_build(&cli)?;

    llvm::build_llvm(&cli)?;
    llvm::build_lld(&cli)?;

    for triple in all_possible_platforms() {
        libc::install_headers(&cli, &triple)?;
        llvm::build_runtimes(&cli, &triple)?;

        libc::build_libc(&cli, &triple)?;

        //libc::build_libcxx(&cli, triple)?;
    }

    return Ok(());

    let path = std::env::var("PATH").unwrap();
    let lld_bin = get_lld_bin(guess_host_triple().unwrap())?;
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}:{}:{}",
            lld_bin.to_string_lossy(),
            std::fs::canonicalize("toolchain/install/bin")
                .unwrap()
                .to_string_lossy(),
            std::fs::canonicalize("toolchain/install/python/bin")
                .unwrap()
                .to_string_lossy(),
            path
        ),
    );

    let current_dir = std::env::current_dir().unwrap();
    let sysroots = Path::new("toolchain/install/sysroots").canonicalize()?;
    std::env::set_var("TWIZZLER_ABI_SYSROOTS", sysroots);

    if !cli.skip_rust {
        println!("starting rust build");
        rust::build_rust(&cli)?;
    }
    return Ok(());

    if cli.native {
        return Ok(());
    }

    println!("rust build finished, packaging toolchain");
    install::install(&cli)?;

    if !cli.skip_prune {
        prune_toolchain()?;
    }

    println!("building runtimes");
    do_post_toolchain_runtime_build(&cli)?;

    println!("toolchain packaging finished, pruning binaries");
    prune_bins()?;

    if cli.compress {
        println!("compressing toolchain");
        compress_toolchain()?;
    }

    println!("ready!");
    Ok(())
}
