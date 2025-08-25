use std::{
    fs::remove_dir_all,
    path::{Path, PathBuf},
    process::Command,
};

use bootstrap::do_bootstrap;
use clap::{Args, Subcommand};
use guess_host_triple::guess_host_triple;
use pathfinding::{get_rustc_path, get_rustdoc_path, get_rustlib_bin};
use reqwest::Client;

use crate::triple::Triple;

mod bootstrap;
mod pathfinding;
mod utils;

pub use pathfinding::*;
pub use utils::*;

#[derive(clap::Args, Debug)]
pub struct BootstrapOptions {
    #[clap(long, help = "Skip downloading boot files from file server.")]
    skip_downloads: bool,
    #[clap(long, help = "Skip compiling the rust toolchain (not recommended...).")]
    skip_rust: bool,
    #[clap(
        long,
        help = "Don't remove the target/ directory after rebuilding the toolchain."
    )]
    keep_old_artifacts: bool,
    #[clap(
        long,
        help = "Keep early stages (0 and 1) of building rustc. Speeds up compilation, but can only be used if you (a) have already done a full bootstrap, and (b) since that bootstrap, all that is modified is twizzler-runtime-api or rust's standard library. Any changes to the compiler require one to not use this flag."
    )]
    keep_early_stages: bool,

    #[clap(long, help = "Skips pruning the toolchain after building")]
    skip_prune: bool,

    #[clap(
        long,
        help = "After bootstrapping, will compress and tag the toolchain for distribution."
    )]
    package: bool,

    #[clap(
        long,
        help = "Compresses the toolchain after bootstrapping for distribution"
    )]
    compress: bool,
}

#[derive(Subcommand, Debug)]
pub enum ToolchainCommands {
    /// Builds the Twizzler toolchain using the checked out submodules
    /// NOTE: ensure that the submodules inside of `/toolchain` are cloned.
    Bootstrap(BootstrapOptions),

    /// Explicitly pull down the toolchain that corresponds with the current submodules
    Pull,

    /// Will delete everything used to build a toolchain
    Prune,

    /// Prints the current active toolchain, if it exists.
    Active,

    /// Prints the tag for the current submodule configuration.
    Tag,

    /// Lists all the installed toolchains.
    List,

    /// Removes toolchains based on the specified options
    Remove(RemoveOptions),

    /// Compresses the active toolchain for distribution
    Compress,
}

#[derive(Args, Debug)]
pub struct RemoveOptions {
    /// Remove a specific toolchain by tag
    #[clap(long)]
    pub tag: Option<String>,

    /// Remove all toolchains
    #[clap(long, conflicts_with = "tag", conflicts_with = "inactive")]
    pub all: bool,

    /// Remove all toolchains but the active one
    #[clap(long, conflicts_with = "tag", conflicts_with = "all")]
    pub inactive: bool,
}

pub fn handle_cli(subcommand: ToolchainCommands) -> anyhow::Result<()> {
    match subcommand {
        ToolchainCommands::Bootstrap(opts) => do_bootstrap(opts),
        ToolchainCommands::Pull => Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(pull_toolchain())?),
        ToolchainCommands::Prune => prune_toolchain(),
        ToolchainCommands::Compress => compress_toolchain(),
        ToolchainCommands::Active => {
            match get_toolchain_path()?.canonicalize() {
                Ok(_) => {
                    println!("{}", generate_tag()?);
                }

                Err(_) => {
                    eprintln!("Active toolchain not found!")
                }
            }
            Ok(())
        }
        ToolchainCommands::Tag => {
            println!("{}", generate_tag()?);
            Ok(())
        }
        ToolchainCommands::List => {
            let active = generate_tag()?;

            for tc in get_installed_toolchains()? {
                if tc == active {
                    println!("Active: {tc}");
                } else {
                    println!("{tc}");
                }
            }

            Ok(())
        }

        ToolchainCommands::Remove(opts) => {
            let rm_tc = |tc_name: &String| -> anyhow::Result<()> {
                let mut tc_path = PathBuf::from("toolchain/");
                tc_path.push(tc_name);
                remove_dir_all(tc_path)?;
                println!("Succesfully removed {}", tc_name);
                Ok(())
            };

            let toolchains = get_installed_toolchains()?;

            if opts.all {
                for tc in &toolchains {
                    rm_tc(tc)?
                }
            }

            if opts.inactive {
                let active_tc = generate_tag()?;

                for tc in &toolchains {
                    if *tc != active_tc {
                        rm_tc(tc)?
                    }
                }
            }

            if let Some(tag) = opts.tag {
                let Some(tc) = toolchains.iter().find(|entry| tag == **entry) else {
                    eprintln!("Toolchain with name {} doesnt exist!", tag);
                    return Ok(());
                };
                rm_tc(tc)?;
                println!("Succesfully removed {}", tc);
            }

            Ok(())
        }
    }
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

async fn download_efi_files(client: &Client) -> anyhow::Result<()> {
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

pub fn set_dynamic(target: &Triple) -> anyhow::Result<()> {
    let sysroot_path = get_sysroots_path(target.to_string().as_str())?;

    // This is a bit of a cursed linker line, but it's needed to work around some limitations in
    // rust's linkage support.
    let args = format!("-C target-feature=+sse3,+avx,+avx2,+fma -C target-cpu=x86-64-v3 -C prefer-dynamic=y -Z staticlib-prefer-dynamic=y -C link-arg=--allow-shlib-undefined -C link-arg=--undefined-glob=__TWIZZLER_SECURE_GATE_* -C link-arg=--export-dynamic-symbol=__TWIZZLER_SECURE_GATE_* -C link-arg=--warn-unresolved-symbols -Z pre-link-arg=-L -Z pre-link-arg={} -L {}", sysroot_path.display(), sysroot_path.display());
    std::env::set_var("RUSTFLAGS", args);
    std::env::set_var("CARGO_TARGET_DIR", "target/dynamic");
    std::env::set_var("TWIZZLER_ABI_SYSROOTS", sysroot_path.canonicalize()?);

    Ok(())
}

pub fn set_static() {
    std::env::set_var(
        "RUSTFLAGS",
        "-C prefer-dynamic=n -Z staticlib-prefer-dynamic=n -C target-feature=+crt-static -C relocation-model=static",
    );
    std::env::set_var("CARGO_TARGET_DIR", "target/static");
}

pub(crate) fn init_for_build(_abi_changes_ok: bool) -> anyhow::Result<()> {
    //TODO: make sure we have the toolchain we need, if not then prompt to build it / error out if
    // its a non-interactive
    //

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(check_toolchain())?;

    let python_path = get_python_path()?.canonicalize()?;
    let builtin_headers = get_builtin_headers()?.canonicalize()?;
    let compiler_rt_path = get_compiler_rt_path()?.canonicalize()?;
    let lld_bin = get_lld_bin(guess_host_triple().unwrap())?.canonicalize()?;
    let rustlib_bin = get_rustlib_bin(guess_host_triple().unwrap())?.canonicalize()?;
    let toolchain_bin = get_bin_path()?.canonicalize()?;
    let path = std::env::var("PATH").unwrap();

    std::env::set_var("RUSTC", &get_rustc_path()?);
    std::env::set_var("RUSTDOC", &get_rustdoc_path()?);
    std::env::set_var("CARGO_CACHE_RUSTC_INFO", "0");
    std::env::set_var("PYTHONPATH", python_path);
    std::env::set_var("TWIZZLER_ABI_BUILTIN_HEADERS", builtin_headers);
    std::env::set_var("RUST_COMPILER_RT_ROOT", compiler_rt_path);

    std::env::set_var(
        "PATH",
        format!(
            "{}:{}:{}:{}",
            rustlib_bin.canonicalize()?.to_string_lossy(),
            lld_bin.canonicalize()?.to_string_lossy(),
            toolchain_bin.canonicalize()?.to_string_lossy(),
            path
        ),
    );
    Ok(())
}

/// Checks if we have a valid toolchain locally, prompts to build it if necessary
async fn check_toolchain() -> anyhow::Result<()> {
    let tc_path = get_toolchain_path()?;
    let exists_locally = tc_path.try_exists()?;

    // check if exists remotely
    let exists_remotely = get_checked_download_url().await.is_ok();
    if !exists_locally {
        eprintln!("There doesnt exist a local toolchain capable of building Twizzler!");
        if exists_remotely {
            eprintln!("Remote toolchain found! Pulling it down.");
            pull_toolchain().await?;
        } else {
            // here we just tell the user what to run
            eprintln!(
                r#"
Remote toolchain doesn't exist!!
Continuing well require a full compilation of the twizzler toolchain!
This operation will require ~40-50 Gb of disk space and will take a substantial amount of time!

Please run

git submodule update --init --recursive
cargo toolchain bootstrap
                "#
            );
        }
    }

    Ok(())
}

pub fn set_cc(target: &Triple) -> anyhow::Result<()> {
    let toolchain_path = get_toolchain_path()?;

    let clang_path = {
        let mut clang_path = toolchain_path.clone();
        clang_path.push("bin/clang");
        clang_path.canonicalize().unwrap()
    };

    // When compiling crates that compile C code (e.g. alloca), we need to use our clang.
    // let clang_path = Path::new(format!("{}/bin/clang", toolchain_path).as_str())
    //     .canonicalize()
    //     .unwrap();
    std::env::set_var("CC", &clang_path);
    std::env::set_var("LD", &clang_path);
    std::env::set_var("CXX", &clang_path);

    // We don't have any real system-include files, but we can provide these extremely simple ones.
    let sysroot_path = Path::new(&format!(
        "{}/sysroots/{}",
        toolchain_path.to_string_lossy(),
        target
    ))
    .canonicalize()
    .unwrap();
    // We don't yet support stack protector. Also, don't pull in standard lib includes, as those may
    // go to the system includes.
    let cflags = format!(
        "-fno-stack-protector -isysroot {} -target {} --sysroot {}",
        sysroot_path.display(),
        target,
        sysroot_path.display(),
    );
    std::env::set_var("CFLAGS", &cflags);
    std::env::set_var("LDFLAGS", &cflags);
    std::env::set_var("CXXFLAGS", &cflags);

    Ok(())
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
