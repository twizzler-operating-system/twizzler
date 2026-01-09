use guess_host_triple::guess_host_triple;

use super::BootstrapOptions;
use crate::toolchain::{compress_toolchain, prune_bins, prune_toolchain};

mod paths;
use paths::*;
mod mover;

mod install;
mod prep;
mod rust;

pub(crate) fn do_bootstrap(cli: BootstrapOptions) -> anyhow::Result<()> {
    prep::setup_build(&cli)?;
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

    //libc::build_libc(&cli)?;

    let current_dir = std::env::current_dir().unwrap();
    let builtin_headers =
        current_dir.join("toolchain/src/rust/build/host/llvm/lib/clang/21/include/");
    std::env::set_var("TWIZZLER_ABI_BUILTIN_HEADERS", builtin_headers);
    std::env::set_var("TWIZZLER_ABI_SYSROOTS", "toolchain/install/sysroots");

    if !cli.skip_rust {
        rust::build_rust(&cli)?;
    }

    install::install(&cli)?;

    if !cli.skip_prune {
        prune_toolchain()?;
    }

    prune_bins()?;

    if cli.compress {
        compress_toolchain()?;
    }

    println!("ready!");
    Ok(())
}
