use std::process::Command;

use fs_extra::dir::CopyOptions;

use crate::{
    toolchain::{
        bootstrap::paths::{get_llvm_native_runtime, get_llvm_native_runtime_install},
        get_toolchain_path, guess_host_triple, BootstrapOptions,
    },
    triple::all_possible_platforms,
};

pub fn install(cli: &BootstrapOptions) -> anyhow::Result<()> {
    tracing::info!("installing LLVM toolchain and native libraries");
    for target in &crate::triple::all_possible_platforms() {
        let target = target.to_string();

        let _ =
            std::fs::create_dir_all(get_llvm_native_runtime_install(&target)?.parent().unwrap());

        std::fs::copy(
            get_llvm_native_runtime(&target)?,
            get_llvm_native_runtime_install(&target)?,
        )?;

        for name in &["crtbegin", "crtend", "crtbeginS", "crtendS"] {
            let src = format!("toolchain/src/rust/build/{}/native/crt/{}.o", &target, name);
            let dst = format!("toolchain/install/lib/clang/21/lib/{}/{}.o", &target, name);
            std::fs::copy(src, dst)?;
        }
        for name in &["crti", "crtn"] {
            let src = format!(
                "toolchain/install/lib/rustlib/{}/lib/self-contained/{}.o",
                &target, name
            );
            let dst = format!("toolchain/install/lib/clang/21/lib/{}/{}.o", &target, name);
            println!("Copy: {} -> {}", src, dst);
            std::fs::copy(src, dst)?;
        }
        let src = format!("toolchain/install/lib/rustlib/{}/lib/libunwind.a", &target);
        let dst = format!("toolchain/install/lib/clang/21/lib/{}/libunwind.a", &target);
        std::fs::copy(src, dst)?;
    }
    let items = ["bin", "include", "lib", "libexec", "share"]
        .into_iter()
        .map(|name| format!("toolchain/src/rust/build/host/llvm/{}", name))
        .collect::<Vec<_>>();

    fs_extra::copy_items(
        &items,
        "toolchain/install",
        &CopyOptions::new().overwrite(true),
    )?;

    tracing::info!("installing libc and C headers");
    for target_triple in all_possible_platforms() {
        let current_dir = std::env::current_dir().unwrap();
        let sysroot_dir = current_dir.join(format!(
            "toolchain/install/sysroots/{}",
            target_triple.to_string()
        ));
        let build_dir_name = format!("build-{}", target_triple.to_string());
        let src_dir = current_dir.join("toolchain/src/mlibc");
        let build_dir = src_dir.join(&build_dir_name);
        //let cross_file = format!("{}/meson-cross-twizzler.txt", sysroot_dir.display());

        let cxx_install_dir = current_dir.join(&format!(
            "toolchain/src/rust/build/{}/native/libcxx",
            target_triple.to_string()
        ));

        let cxxabi_install_dir = current_dir.join(&format!(
            "toolchain/src/rust/build/{}/native/libcxxabi",
            target_triple.to_string()
        ));
        let sysroot_include = sysroot_dir.join("include");
        let sysroot_lib = sysroot_dir.join("lib");

        std::fs::create_dir_all(&sysroot_lib)?;

        let status = Command::new("cp")
            .arg("-R")
            .arg(cxx_install_dir.join("include/c++"))
            .arg(&sysroot_include)
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to copy C++ headers");
        }
        let status = Command::new("cp")
            .arg("-R")
            .arg(cxxabi_install_dir.join("include/c++"))
            .arg(&sysroot_include)
            .status()?;
        if !status.success() {
            anyhow::bail!("failed to copy C++ ABI headers");
        }

        std::fs::copy(
            cxx_install_dir.join("lib/libc++.a"),
            sysroot_lib.join("libc++.a"),
        )?;
        std::fs::copy(
            cxx_install_dir.join("lib/libc++experimental.a"),
            sysroot_lib.join("jibc++experimental.a"),
        )?;
        std::fs::copy(
            cxxabi_install_dir.join("lib/libc++abi.a"),
            sysroot_lib.join("libc++abi.a"),
        )?;
        std::fs::copy(
            cxxabi_install_dir.join("lib/libc++abi.so"),
            sysroot_lib.join("libc++abi.so"),
        )?;

        let _ = std::fs::remove_dir_all(&build_dir);

        let usr_link = sysroot_dir.join("usr");
        let _ = std::fs::remove_file(&usr_link);
        std::os::unix::fs::symlink(".", usr_link)?;
    }

    if !cli.keep_old_artifacts {
        let res = std::fs::remove_dir_all("target");
        if let Err(e) = res {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!("warning -- failed to remove old build artifacts: {}", e);
            }
        }
    }

    let host_triple = guess_host_triple().unwrap();

    for target_triple in all_possible_platforms() {
        crate::toolchain::bootstrap::mover::move_all(host_triple, &target_triple.to_string())?;
    }

    let usr_link = format!("{}/usr", get_toolchain_path()?.display());
    let local_link = format!("{}/local", get_toolchain_path()?.display());
    let _ = std::fs::remove_file(&usr_link);
    std::os::unix::fs::symlink(".", &usr_link)?;
    let _ = std::fs::remove_file(&local_link);
    std::os::unix::fs::symlink(".", &local_link)?;

    Ok(())
}
