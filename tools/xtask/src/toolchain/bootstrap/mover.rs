use std::{env::current_dir, path::PathBuf, process::Command};

use super::paths as bootstrap;
use crate::toolchain::{get_toolchain_path, pathfinding};

pub fn move_all(host_triple: &str, target_triple: &str) -> anyhow::Result<()> {
    let move_dir = |prev: PathBuf, next: PathBuf| -> anyhow::Result<()> {
        println!("Moving {} to {}", prev.display(), next.display());

        // remove dest if it exists
        if next.exists() {
            let _ = std::fs::remove_dir_all(&next);
        }

        if let Some(parent) = next.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let status = Command::new("cp").arg("-r").arg(&prev).arg(&next).status(); // Use status() instead of spawn() to wait for completion

        //NOTE: copy will fail if there are recursive symlinks, in this case we force move
        if status.is_err() {
            let status = Command::new("mv").arg(&prev).arg(&next).status()?; // Use status() instead of
            if !status.success() {
                anyhow::bail!("mv command failed with status: {}", status);
            }
        }

        Ok(())
    };

    // first we just move the install directory
    let old_install_dir = {
        let mut x = current_dir()?;
        x.push("toolchain/install");
        x
    };

    let new_install_dir = get_toolchain_path()?;
    move_dir(old_install_dir.clone(), new_install_dir)?;

    // llvm native runtime
    let old_llvm_rt = bootstrap::get_llvm_native_runtime(target_triple)?;
    let new_llvm_rt = pathfinding::get_llvm_native_runtime_install(target_triple)?;
    move_dir(old_llvm_rt, new_llvm_rt)?;

    // llvm native runtime install
    let old_llvm_native_install = bootstrap::get_llvm_native_runtime(target_triple)?;
    let new_llvm_native_install = pathfinding::get_llvm_native_runtime_install(target_triple)?;
    move_dir(old_llvm_native_install, new_llvm_native_install)?;

    // rust stage2 std
    let old_rust_stage_2 = bootstrap::get_rust_stage2_std(host_triple, target_triple)?;
    let new_rust_stage_2 = pathfinding::get_rust_stage2_std(host_triple, target_triple)?;
    move_dir(old_rust_stage_2, new_rust_stage_2)?;

    // rust lld
    let old_rust_lld = bootstrap::get_rust_lld(host_triple)?;
    let new_rust_lld = pathfinding::get_rust_lld(host_triple)?;
    move_dir(old_rust_lld, new_rust_lld)?;

    //compiler_rt
    let old_compiler_rt = bootstrap::get_compiler_rt_path()?;
    let new_compiler_rt = pathfinding::get_compiler_rt_path()?;
    move_dir(old_compiler_rt, new_compiler_rt)?;

    // lld bin
    let old_lld_bin = bootstrap::get_lld_bin(host_triple)?;
    let new_lld_bin = pathfinding::get_lld_bin(host_triple)?;
    move_dir(old_lld_bin, new_lld_bin)?;

    Ok(())
}
