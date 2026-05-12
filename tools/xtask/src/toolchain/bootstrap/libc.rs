use std::{fs::File, io::Write, path::Path, process::Command};

use crate::{
    toolchain::{
        bootstrap::llvm::{setup_cmake, setup_cmake_twizzler},
        BootstrapOptions,
    },
    triple::Triple,
};

pub fn install_headers(_cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    println!("== Installing libc headers for {}", triple);

    let install_path = Path::new("toolchain/install");
    let mlibc_src = Path::new("toolchain/src/mlibc").canonicalize()?;
    let build_dir_name = format!("build-{}", triple);
    let build_dir = Path::new("toolchain/build/mlibc").join(&build_dir_name);

    let mlibc_sysroot = install_path.join(format!("sysroots/{}", triple));
    let cross_file = format!("{}/meson-cross-twizzler.txt", mlibc_sysroot.display());

    std::fs::create_dir_all(&install_path)?;
    std::fs::create_dir_all(&build_dir)?;
    std::fs::create_dir_all(&mlibc_sysroot)?;
    let install_path = install_path.canonicalize()?;
    let mlibc_sysroot = mlibc_sysroot.canonicalize()?;
    let build_dir = build_dir.canonicalize()?;

    let mut cf = File::create(&cross_file)?;
    let cross_file = Path::new(&cross_file).canonicalize()?;

    let llvm_bin_path = install_path.join("bin").canonicalize()?;
    writeln!(&mut cf, "[binaries]")?;
    for tool in [
        ("c", "clang"),
        ("cpp", "clang++"),
        ("ar", "llvm-ar"),
        ("strip", "llvm-strip"),
    ] {
        let path = llvm_bin_path.join(tool.1);
        writeln!(&mut cf, "{} = '{}'", tool.0, path.display())?;
    }

    writeln!(&mut cf, "[built-in options]")?;
    for tool in ["c_args", "c_link_args", "cpp_args", "cpp_link_args"] {
        write!(
            &mut cf,
            "{} = ['-B{}', '-isysroot', '{}', '--sysroot', '{}', '-target', '{}', ",
            tool,
            llvm_bin_path.display(),
            mlibc_sysroot.display(),
            mlibc_sysroot.display(),
            triple,
        )?;
        if tool == "c_link_args" || tool == "cpp_link_args" {
            write!(&mut cf, "'-z', 'norelro'",)?;
        }
        writeln!(&mut cf, "]")?;
    }

    writeln!(&mut cf, "[properties]")?;
    writeln!(&mut cf, "sys_root = '{}'", mlibc_sysroot.display())?;

    writeln!(&mut cf, "[host_machine]")?;
    writeln!(&mut cf, "system = 'twizzler'")?;
    writeln!(
        &mut cf,
        "cpu_family = '{}'",
        triple.to_string().split("-").next().unwrap()
    )?;
    writeln!(
        &mut cf,
        "cpu = '{}'",
        triple.to_string().split("-").next().unwrap()
    )?;
    writeln!(&mut cf, "endian = 'little'")?;
    drop(cf);

    let _ = std::fs::remove_dir_all(&build_dir);

    let status = Command::new("toolchain/install/python/bin/meson")
        .arg("setup")
        .arg(format!("-Dprefix={}", mlibc_sysroot.display()))
        .arg("-Dheaders_only=true")
        .arg("-Ddefault_library=both")
        .arg("-Dlibgcc_dependency=true")
        .arg("-Duse_freestnd_hdrs=enabled")
        .arg(format!("--cross-file={}", cross_file.display()))
        .arg("--buildtype=debugoptimized")
        .arg(&build_dir)
        .arg(&mlibc_src)
        .status()?;
    if !status.success() {
        Err(std::io::Error::other(
            "failed to setup meson for libc headers",
        ))?;
    }

    let status = Command::new("toolchain/install/python/bin/meson")
        .arg("compile")
        .arg("-C")
        .arg(&build_dir)
        .status()?;
    if !status.success() {
        Err(std::io::Error::other("failed to build libc headers"))?;
    }

    let status = Command::new("toolchain/install/python/bin/meson")
        .arg("install")
        .arg("-q")
        .arg("-C")
        .arg(&build_dir)
        .status()?;
    if !status.success() {
        Err(std::io::Error::other("failed to install libc headers"))?;
    }

    Ok(())
}

pub fn build_libc(_cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    println!("== Building libc for {}", triple);

    let install_path = Path::new("toolchain/install");
    let mlibc_src = Path::new("toolchain/src/mlibc").canonicalize()?;
    let build_dir_name = format!("build-{}", triple);
    let build_dir = Path::new("toolchain/build/mlibc").join(&build_dir_name);
    let _ = fs_extra::dir::remove(&build_dir);

    let mlibc_sysroot = install_path.join(format!("sysroots/{}", triple));
    let cross_file = format!("{}/meson-cross-twizzler.txt", mlibc_sysroot.display());

    std::fs::create_dir_all(&install_path)?;
    std::fs::create_dir_all(&build_dir)?;
    std::fs::create_dir_all(&mlibc_sysroot)?;
    let _install_path = install_path.canonicalize()?;
    let mlibc_sysroot = mlibc_sysroot.canonicalize()?;
    let build_dir = build_dir.canonicalize()?;

    let status = Command::new("toolchain/install/python/bin/meson")
        .arg("setup")
        .arg(format!("-Dprefix={}", mlibc_sysroot.display()))
        .arg("-Dheaders_only=false")
        .arg("-Ddefault_library=both")
        .arg("-Dlibgcc_dependency=true")
        .arg("-Duse_freestnd_hdrs=enabled")
        .arg(format!("--cross-file={}", cross_file))
        .arg("--buildtype=debugoptimized")
        .arg(&build_dir)
        .arg(&mlibc_src)
        .status()?;
    if !status.success() {
        Err(std::io::Error::other("failed to setup meson for libc"))?;
    }

    let status = Command::new("toolchain/install/python/bin/meson")
        .arg("compile")
        .arg("-C")
        .arg(&build_dir)
        .status()?;
    if !status.success() {
        Err(std::io::Error::other("failed to build libc"))?;
    }

    let status = Command::new("toolchain/install/python/bin/meson")
        .arg("install")
        .arg("-q")
        .arg("-C")
        .arg(&build_dir)
        .status()?;
    if !status.success() {
        Err(std::io::Error::other("failed to install libc"))?;
    }

    Ok(())
}

pub fn build_libcxx(cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    println!("== Building libcxx for {}", triple);
    let install_path = Path::new("toolchain/install/sysroots").join(&triple.to_string());
    let src_path = Path::new("toolchain/src/rust/src/llvm-project/libcxx").canonicalize()?;
    let build_dir_name = format!("build-{}", triple);
    let build_dir = Path::new("toolchain/build/libcxx").join(&build_dir_name);
    let libcxxabi_build_dir = Path::new("toolchain/build/libcxxabi").join(&build_dir_name);
    let _ = fs_extra::dir::remove(&build_dir);

    std::fs::create_dir_all(&install_path)?;
    let install_path = install_path.canonicalize()?;
    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;
    std::fs::create_dir_all(&libcxxabi_build_dir)?;
    let libcxxabi_build_dir = libcxxabi_build_dir.canonicalize()?;

    let mut cfg = cmake::Config::new(&src_path);
    cfg.profile("Release");
    cfg.define("CMAKE_C_COMPILER_TARGET", triple.to_string());
    cfg.define("CMAKE_CXX_COMPILER_TARGET", triple.to_string());

    setup_cmake(&mut cfg, Some(install_path.as_path()))?;
    setup_cmake_twizzler(
        &mut cfg,
        triple,
        vec![
            "-nostdlib".to_string(),
            "-nostdlibinc".to_string(),
            "-I".to_string(),
            libcxxabi_build_dir
                .join("build/include/c++/v1")
                .display()
                .to_string(),
            "-I".to_string(),
            install_path.join("include").display().to_string(),
        ],
    )?;

    let llvm_cmake_dir = Path::new("toolchain/install/lib/cmake/llvm").canonicalize()?;
    let llvm_config = install_path.join("bin/llvm-config");
    cfg.define("LLVM_CMAKE_DIR", llvm_cmake_dir.display().to_string())
        .define("LLVM_INCLUDE_TESTS", "OFF");
    cfg.define("LLVM_CONFIG_PATH", &llvm_config);
    cfg.define("LIBCXX_HAS_PTHREAD_API", "ON");
    cfg.define("LIBCXX_CXX_ABI", "libcxxabi");
    cfg.define("LIBCXX_ENABLE_UNICODE", "ON");
    cfg.define("LIBCXX_ENABLE_SHARED", "OFF");
    cfg.define("LIBCXX_ENABLE_EXCEPTIONS", "ON");
    cfg.define("LIBCXX_ENABLE_RTTI", "ON");
    cfg.define("LIBCXX_ENABLE_WIDE_CHARACTERS", "ON");
    cfg.define("_LIBCPP_NO_VCRUNTIME", "ON");
    cfg.define("LIBCXX_STATICALLY_LINK_ABI_IN_SHARED_LIBRARY", "OFF");

    cfg.out_dir(&build_dir);
    cfg.build_target("libcxx-generate-files");
    cfg.build();

    cfg.build_target("install-cxx-modules");
    cfg.build();

    cfg.build_target("install-cxx-headers");
    cfg.build();
    build_libcxxabi(cli, triple)?;

    cfg.build_target("install-cxx");
    cfg.build();

    Ok(())
}

fn build_libcxxabi(_cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    println!("== Building libcxxabi for {}", triple);
    let install_path = Path::new("toolchain/install/sysroots").join(&triple.to_string());
    let src_path = Path::new("toolchain/src/rust/src/llvm-project/libcxxabi").canonicalize()?;
    let build_dir_name = format!("build-{}", triple);
    let build_dir = Path::new("toolchain/build/libcxxabi").join(&build_dir_name);
    let _ = fs_extra::dir::remove(&build_dir);
    let libcxx_build_dir = Path::new("toolchain/build/libcxx")
        .join(&build_dir_name)
        .canonicalize()?;

    std::fs::create_dir_all(&install_path)?;
    let install_path = install_path.canonicalize()?;
    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    let mut cfg = cmake::Config::new(&src_path);
    cfg.profile("Release");

    std::fs::create_dir_all(&install_path)?;
    let install_path = install_path.canonicalize()?;
    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    let mut cfg = cmake::Config::new(&src_path);
    cfg.profile("Release");
    cfg.define("CMAKE_C_COMPILER_TARGET", triple.to_string());
    cfg.define("CMAKE_CXX_COMPILER_TARGET", triple.to_string());

    setup_cmake(&mut cfg, Some(install_path.as_path()))?;
    setup_cmake_twizzler(
        &mut cfg,
        triple,
        vec![
            "-nostdlib".to_string(),
            "-nostdlibinc".to_string(),
            "-I".to_string(),
            libcxx_build_dir
                .join("build/include/c++/v1")
                .display()
                .to_string(),
            "-I".to_string(),
            install_path.join("include").display().to_string(),
        ],
    )?;

    let llvm_cmake_dir = Path::new("toolchain/install/lib/cmake/llvm").canonicalize()?;
    let llvm_config = install_path.join("bin/llvm-config");
    cfg.define("LLVM_CMAKE_DIR", llvm_cmake_dir.display().to_string())
        .define("LLVM_INCLUDE_TESTS", "OFF");
    cfg.define("LLVM_CONFIG_PATH", &llvm_config);
    cfg.define("_LIBCPP_NO_VCRUNTIME", "ON");
    cfg.define("LIBCXXABI_USE_LLVM_UNWINDER", "OFF");
    cfg.define("LIBCXXABI_ENABLE_THREADS", "ON");
    cfg.define("LIBCXXABI_ENABLE_EXCEPTIONS", "ON");

    cfg.out_dir(&build_dir);

    cfg.build_target("cxxabi_static");
    cfg.build();

    cfg.build_target("cxxabi_shared");
    cfg.build();

    cfg.build_target("install-cxxabi-headers");
    cfg.build();

    cfg.build_target("install-cxxabi");
    cfg.build();

    //   eprintln!("INS CX");
    //  cfg.build_target("install-cxx");
    //  cfg.build();
    //

    Ok(())
}
