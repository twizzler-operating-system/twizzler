use std::path::Path;

use guess_host_triple::guess_host_triple;

use crate::triple::Triple;
pub fn install(triple: &Triple) -> anyhow::Result<()> {
    println!("Building llvm for {}", triple);
    build_llvm(triple)?;
    build_lld(triple)?;

    Ok(())
}

fn setup_cmake(cfg: &mut cmake::Config, install_path: Option<&Path>) -> anyhow::Result<()> {
    cfg.define("CMAKE_INSTALL_MESSAGE", "LAZY");
    if let Some(install_path) = install_path {
        std::fs::create_dir_all(&install_path)?;
        let install_path = install_path.canonicalize()?;
        cfg.define("CMAKE_INSTALL_PREFIX", install_path.display().to_string());
    } else {
        cfg.env("DESTDIR", "");
    }
    cfg.generator("Ninja");
    let nr_jobs = std::thread::available_parallelism()?.to_string();
    cfg.build_arg("-j").build_arg(&nr_jobs);
    cfg.profile("Release");
    cfg.host(guess_host_triple().unwrap());

    Ok(())
}

fn setup_cmake_twizzler(
    cfg: &mut cmake::Config,
    target: &Triple,
    mut passed_c_flags: Vec<String>,
) -> anyhow::Result<()> {
    let bin_dir = Path::new("toolchain/install/bin").canonicalize()?;
    let sysroot = Path::new("toolchain/install/sysroots")
        .join(&target.to_string())
        .canonicalize()?;
    let cc = bin_dir.join("clang");
    let cxx = bin_dir.join("clang++");
    let ld = bin_dir.join("clang");
    let ar = bin_dir.join("llvm-ar");
    let ranlib = bin_dir.join("llvm-ranlib");
    let _llvm_config = bin_dir.join("llvm-config");
    cfg.target(&target.to_string());
    cfg.define("CMAKE_SYSTEM_NAME", "Twizzler");
    cfg.define("TWIZZLER", "True");
    cfg.define("CMAKE_CROSSCOMPILING", "True");
    cfg.define("CMAKE_C_COMPILER", &cc);
    cfg.define("CMAKE_CXX_COMPILER", &cxx);
    cfg.define("CMAKE_ASM_COMPILER", &cc);
    cfg.define("CMAKE_AR", &ar);
    cfg.define("CMAKE_LD", &ld);
    cfg.define("CMAKE_RANLIB", &ranlib);

    cfg.define("CMAKE_SYSROOT", sysroot.display().to_string());
    cfg.define("CMAKE_FIND_ROOT_PATH_MODE_PROGRAM", "NEVER");
    cfg.define("CMAKE_FIND_ROOT_PATH_MODE_LIBRARY", "ONLY");
    cfg.define("CMAKE_FIND_ROOT_PATH_MODE_INCLUDE", "ONLY");
    cfg.define("CMAKE_FIND_ROOT_PATH_MODE_PACKAGE", "ONLY");
    cfg.define("CMAKE_C_COMPILER_TARGET", target.to_string());
    cfg.define("CMAKE_CXX_COMPILER_TARGET", target.to_string());

    let mut cflags = Vec::new();
    cflags.push(format!("--target={}", target.to_string()));
    cflags.push(format!("--sysroot={}", sysroot.display()));
    cflags.push(" -D__Twizzler__".to_string());
    cflags.append(&mut passed_c_flags);
    cfg.define("CMAKE_C_FLAGS", cflags.join(" "));
    cfg.define("CMAKE_CXX_FLAGS", cflags.join(" "));

    Ok(())
}

fn build_llvm(triple: &Triple) -> anyhow::Result<()> {
    let llvm_src_path = Path::new("toolchain/src/rust/src/llvm-project").canonicalize()?;
    let llvm_build_path = Path::new("toolchain/build/ports/llvm").join(triple.to_string());
    let llvm_install_path = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .join("pkg/llvm");
    let nr_jobs = std::thread::available_parallelism()?.to_string();

    std::fs::create_dir_all(&llvm_build_path)?;
    std::fs::create_dir_all(&llvm_install_path)?;
    let llvm_install_path = llvm_install_path.canonicalize()?;
    let llvm_build_path = llvm_build_path.canonicalize()?;
    let mut cfg = cmake::Config::new(llvm_src_path.join("llvm"));

    cfg.out_dir(&llvm_build_path)
        .profile("Release")
        .define("LLVM_ENABLE_ASSERTIONS", "ON")
        .define("LLVM_UNREACHABLE_OPTIMIZE", "OFF")
        .define("LLVM_ENABLE_PLUGINS", "")
        .define("LLVM_TARGETS_TO_BUILD", "AArch64;X86")
        .define("LLVM_EXPERIMENTAL_TARGETS_TO_BUILD", "")
        .define("LLVM_INCLUDE_EXAMPLES", "OFF")
        .define("LLVM_INCLUDE_DOCS", "OFF")
        .define("LLVM_INCLUDE_BENCHMARKS", "OFF")
        .define("LLVM_INCLUDE_TESTS", "OFF")
        .define("LLVM_ENABLE_LIBEDIT", "OFF")
        .define("LLVM_ENABLE_BINDINGS", "OFF")
        .define("LLVM_ENABLE_Z3_SOLVER", "OFF")
        .define("LLVM_ENABLE_LIBXML2", "OFF")
        .define("LLVM_PARALLEL_COMPILE_JOBS", &nr_jobs)
        .define(
            "LLVM_TARGET_ARCH",
            triple.to_string().split('-').next().unwrap(),
        )
        .define("LLVM_DEFAULT_TARGET_TRIPLE", triple.to_string())
        .define("LLVM_ENABLE_WARNINGS", "OFF");

    cfg.define("LLVM_INSTALL_UTILS", "ON");
    cfg.define("LLVM_ENABLE_ZLIB", "OFF");

    cfg.define("LLVM_ENABLE_RUNTIMES", "");
    cfg.define("LLVM_ENABLE_PROJECTS", "clang");
    cfg.define("LLVM_VERSION_SUFFIX", "-rust-twizzler");

    cfg.target(&triple.to_string()).host(&triple.to_string());
    cfg.define("LLVM_ENABLE_ZSTD", "OFF");

    setup_cmake(&mut cfg, Some(&llvm_install_path))?;
    setup_cmake_twizzler(&mut cfg, triple, vec![])?;

    let _res = cfg.build();

    Ok(())
}

pub fn build_lld(triple: &Triple) -> anyhow::Result<()> {
    println!("== Building linker for {}", triple);

    let lld_dir = Path::new("toolchain/src/rust/src/llvm-project/lld").canonicalize()?;
    let lld_install_path = Path::new("toolchain/install/sysroots")
        .join(triple.to_string())
        .join("pkg/lld");
    let build_dir = Path::new("toolchain/build/ports/lld").join(&triple.to_string());
    let llvm_cmake_dir = Path::new("toolchain/install/sysroots")
        .join(&triple.to_string())
        .join("pkg/llvm/lib/cmake/llvm")
        .canonicalize()?;

    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    std::fs::create_dir_all(&lld_install_path)?;
    let lld_install_path = lld_install_path.canonicalize()?;
    std::fs::create_dir_all(&lld_install_path.join("bin"))?;

    let mut cfg = cmake::Config::new(lld_dir);

    setup_cmake(&mut cfg, None)?;
    setup_cmake_twizzler(&mut cfg, triple, vec![])?;

    cfg.out_dir(&build_dir)
        .define("LLVM_CMAKE_DIR", &llvm_cmake_dir)
        .define("LLVM_DIR", &llvm_cmake_dir)
        .define("LLVM_ENABLE_LIBXML2", "OFF")
        .define("LLVM_INCLUDE_TESTS", "OFF");

    cfg.build();

    std::fs::copy(
        build_dir.join("bin/ld.lld"),
        lld_install_path.join("bin/ld.lld"),
    )?;
    std::fs::copy(build_dir.join("bin/lld"), lld_install_path.join("bin/lld"))?;
    std::fs::copy(
        build_dir.join("bin/ld64.lld"),
        lld_install_path.join("bin/ld64.lld"),
    )?;
    std::fs::copy(
        build_dir.join("bin/lld-link"),
        lld_install_path.join("bin/lld-link"),
    )?;
    std::fs::copy(
        build_dir.join("bin/wasm-ld"),
        lld_install_path.join("bin/wasm-ld"),
    )?;

    Ok(())
}
