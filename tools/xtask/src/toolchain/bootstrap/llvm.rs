use std::{ffi::OsStr, path::Path};

use guess_host_triple::guess_host_triple;

use crate::{
    toolchain::BootstrapOptions,
    triple::{all_possible_platforms, Triple},
};

pub fn setup_cmake(cfg: &mut cmake::Config, install_path: Option<&Path>) -> anyhow::Result<()> {
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

pub fn setup_cmake_twizzler(
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
    let ranlib = bin_dir.join("llvm-ar");
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

pub fn build_llvm(_cli: &BootstrapOptions) -> anyhow::Result<()> {
    let llvm_src_path = Path::new("toolchain/src/rust/src/llvm-project").canonicalize()?;
    let target_native = guess_host_triple().unwrap();
    let llvm_build_path = Path::new("toolchain/build/llvm").join(target_native);
    let nr_jobs = std::thread::available_parallelism()?.to_string();

    std::fs::create_dir_all(&llvm_build_path)?;
    std::fs::create_dir_all("toolchain/install")?;
    let llvm_build_path = llvm_build_path.canonicalize()?;

    if std::fs::exists(llvm_build_path.join("bin/llvm-config"))? {
        println!("LLVM is already built");
        //return Ok(());
    }

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
        .define("LLVM_PARALLEL_COMPILE_JOBS", &nr_jobs)
        .define("LLVM_TARGET_ARCH", target_native.split('-').next().unwrap())
        .define("LLVM_DEFAULT_TARGET_TRIPLE", target_native)
        .define("LLVM_ENABLE_WARNINGS", "OFF");

    cfg.define("LLVM_INSTALL_UTILS", "ON");
    cfg.define("LLVM_ENABLE_ZLIB", "OFF");

    cfg.define("LLVM_ENABLE_RUNTIMES", "");
    cfg.define("LLVM_ENABLE_PROJECTS", "clang");
    cfg.define("LLVM_VERSION_SUFFIX", "-rust-twizzler");

    cfg.target(target_native).host(target_native);
    cfg.define("LLVM_ENABLE_ZSTD", "OFF");

    setup_cmake(
        &mut cfg,
        Some(Path::new("toolchain/install").canonicalize()?.as_path()),
    )?;

    let _res = cfg.build();

    for triple in all_possible_platforms() {
        let ls_name = format!("{}_linker_script.ld", triple.to_string().replace("-", "_"));
        let install_dir = Path::new("toolchain/install/sysroots")
            .join(&triple.to_string())
            .join("lib");
        std::fs::create_dir_all(&install_dir)?;
        let install_dir = install_dir.canonicalize()?;
        std::fs::copy(
            Path::new("toolchain/src").join(ls_name),
            install_dir.join("twizzler.ld"),
        )?;
    }

    Ok(())
}

pub fn build_lld(_cli: &BootstrapOptions) -> anyhow::Result<()> {
    let triple = guess_host_triple().unwrap();
    println!("== Building linker");

    let lld_dir = Path::new("toolchain/src/rust/src/llvm-project/lld").canonicalize()?;
    let bin_dir = Path::new("toolchain/install/bin");
    let build_dir = Path::new("toolchain/build/lld").join(&triple.to_string());
    let llvm_cmake_dir = Path::new("toolchain/install/lib/cmake/llvm").canonicalize()?;
    let _llvm_config = bin_dir.join("llvm-config");

    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    std::fs::create_dir_all(&bin_dir)?;
    let bin_dir = bin_dir.canonicalize()?;

    let mut cfg = cmake::Config::new(lld_dir);

    setup_cmake(&mut cfg, None)?;
    cfg.target(triple);

    cfg.out_dir(&build_dir)
        .define("LLVM_CMAKE_DIR", llvm_cmake_dir)
        .define("LLVM_INCLUDE_TESTS", "OFF");

    cfg.build();

    std::fs::copy(build_dir.join("bin/ld.lld"), bin_dir.join("ld.lld"))?;
    std::fs::copy(build_dir.join("bin/lld"), bin_dir.join("lld"))?;
    std::fs::copy(build_dir.join("bin/ld64.lld"), bin_dir.join("ld64.lld"))?;
    std::fs::copy(build_dir.join("bin/lld-link"), bin_dir.join("lld-link"))?;
    std::fs::copy(build_dir.join("bin/wasm-ld"), bin_dir.join("wasm-ld"))?;

    Ok(())
}

pub fn build_crtbeginend(_cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    println!("==> Building crtbegin/end for {}", triple);
    let install_dir = Path::new("toolchain/install/sysroots")
        .join(&triple.to_string())
        .join("lib");
    let build_dir = Path::new("toolchain/build/crtstuff").join(&triple.to_string());
    let bin_dir = Path::new("toolchain/install/bin").canonicalize()?;
    let src_dir = Path::new("toolchain/src/rust/src/llvm-project/compiler-rt/lib/builtins");

    std::fs::create_dir_all(&install_dir)?;
    let install_dir = install_dir.canonicalize()?;

    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    let mut cfg = cc::Build::new();

    cfg.archiver(bin_dir.join("llvm-ar").as_os_str());

    cfg.compiler(bin_dir.join("clang").as_os_str());
    cfg.cargo_metadata(false)
        .out_dir(&build_dir)
        .target(&triple.to_string())
        .host(guess_host_triple().unwrap())
        .warnings(false)
        .pic(true)
        .debug(false)
        .opt_level(3)
        .file(src_dir.join("crtbegin.c"))
        .file(src_dir.join("crtend.c"));

    // Those flags are defined in src/llvm-project/compiler-rt/lib/builtins/CMakeLists.txt
    // Currently only consumer of those objects is musl, which use .init_array/.fini_array
    // instead of .ctors/.dtors
    cfg.flag("-std=c11")
        .define("CRT_HAS_INITFINI_ARRAY", None)
        .define("EH_USE_FRAME_REGISTRY", None);

    cfg.flag("-nostdlibinc");
    cfg.flag("-nostdlib");

    let objs = cfg.compile_intermediates();
    for obj in objs {
        let (name, name2) = if obj
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("crtbegin.o")
        {
            ("crtbegin.o", "crtbeginS.o")
        } else {
            ("crtend.o", "crtendS.o")
        };
        std::fs::copy(&obj, install_dir.join(name))?;
        std::fs::copy(&obj, install_dir.join(name2))?;
    }

    Ok(())
}

pub fn build_runtimes(cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    build_crtbeginend(cli, triple)?;
    build_libunwind(cli, triple)?;
    build_compiler_rt(cli, triple)?;

    Ok(())
}

pub fn build_libunwind(_cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    println!("== Compiling libunwind for {}", triple);

    let build_dir = Path::new("toolchain/build/libunwind").join(&triple.to_string());
    let install_dir = Path::new("toolchain/install/sysroots").join(&triple.to_string());
    let src_dir = Path::new("toolchain/src/rust/src/llvm-project/libunwind");

    std::fs::create_dir_all(&build_dir)?;
    std::fs::create_dir_all(&install_dir)?;
    let src_dir = src_dir.canonicalize()?;
    let build_dir = build_dir.canonicalize()?;
    let install_dir = install_dir.canonicalize()?;

    let mut cc_cfg = cc::Build::new();
    let mut cpp_cfg = cc::Build::new();

    cpp_cfg.cpp(true);
    cpp_cfg.cpp_set_stdlib(None);
    cpp_cfg.flag("-nostdinc++");
    cpp_cfg.flag("-fno-exceptions");
    cpp_cfg.flag("-fno-rtti");
    cpp_cfg.flag_if_supported("-fvisibility-global-new-delete-hidden");

    let bin_path = Path::new("toolchain/install/bin");
    std::fs::create_dir_all(&bin_path)?;
    let bin_path = bin_path.canonicalize()?;
    let host_triple = guess_host_triple().unwrap();

    for cfg in [&mut cc_cfg, &mut cpp_cfg].iter_mut() {
        cfg.archiver(bin_path.join("llvm-ar"));
        cfg.target(&triple.to_string());
        cfg.host(host_triple);
        cfg.warnings(false);
        cfg.debug(false);
        // get_compiler() need set opt_level first.
        cfg.opt_level(3);
        cfg.flag("-fstrict-aliasing");
        cfg.flag("-funwind-tables");
        cfg.flag("-fvisibility=hidden");
        cfg.define("_LIBUNWIND_DISABLE_VISIBILITY_ANNOTATIONS", None);
        cfg.define("_LIBUNWIND_IS_NATIVE_ONLY", "1");
        cfg.include(src_dir.join("include"));
        cfg.cargo_metadata(false);
        cfg.out_dir(&build_dir);

        //cfg.define("_LIBUNWIND_HAS_NO_THREADS", None);
        cfg.define("_LIBUNWIND_REMEMBER_STACK_ALLOC", None);
        // FIXME (dbittman): This is a hack to get bootstrap headers included when compiling
        // libunwind. We build clang as part of building llvm when bootstrapping the
        // Twizzler toolchain, but it will be unable to find any libc headers (since
        // there aren't any). Fortunately, libunwind is the only part of the
        // Twizzler toolchain build that needs system headers, it needs very few. So
        // we just provide some hacky ones.
        cfg.flag("-nostdlibinc");
        cfg.flag(&format!("--sysroot={}", triple.to_string()));
        cfg.include(install_dir.join("include").canonicalize()?);
        cfg.flag("-fno-stack-protector");
        cfg.define("__ELF__", None);
        //cfg.define("NDEBUG", None);
        //cfg.define("_LIBUNWIND_NO_HEAP", None);
        cfg.define("_LIBUNWIND_USE_DLADDR", Some("0"));
    }

    cc_cfg.compiler(bin_path.join("clang").canonicalize()?);
    cpp_cfg.compiler(bin_path.join("clang++").canonicalize()?);

    let c_sources = vec![
        "Unwind-sjlj.c",
        "UnwindLevel1-gcc-ext.c",
        "UnwindLevel1.c",
        "UnwindRegistersRestore.S",
        "UnwindRegistersSave.S",
    ];

    let cpp_sources = vec!["Unwind-EHABI.cpp", "Unwind-seh.cpp", "libunwind.cpp"];
    let cpp_len = cpp_sources.len();

    for src in c_sources {
        cc_cfg.file(src_dir.join("src").join(src).canonicalize().unwrap());
    }

    for src in &cpp_sources {
        cpp_cfg.file(src_dir.join("src").join(src).canonicalize().unwrap());
    }

    cpp_cfg.compile("unwind-cpp");

    // FIXME: https://github.com/alexcrichton/cc-rs/issues/545#issuecomment-679242845
    let mut count = 0;
    let mut files = std::fs::read_dir(&build_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path().canonicalize().unwrap())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        if file.is_file() && file.extension() == Some(OsStr::new("o")) {
            // Object file name without the hash prefix is "Unwind-EHABI", "Unwind-seh" or
            // "libunwind".
            let base_name = unhashed_basename(&file);
            if cpp_sources.iter().any(|f| *base_name == f[..f.len() - 4]) {
                cc_cfg.object(&file);
                count += 1;
            }
        }
    }
    assert_eq!(cpp_len, count, "Can't get object files from {build_dir:?}");

    cc_cfg.compile("unwind");
    std::fs::create_dir_all(install_dir.join("lib"))?;
    std::fs::copy(
        build_dir.join("libunwind.a"),
        install_dir.join("lib/libunwind.a"),
    )?;

    Ok(())
}

pub fn build_compiler_rt(_cli: &BootstrapOptions, triple: &Triple) -> anyhow::Result<()> {
    println!("== Building compiler-rt for {}", triple);

    let compiler_rt_dir =
        Path::new("toolchain/src/rust/src/llvm-project/compiler-rt").canonicalize()?;
    let install_dir = Path::new("toolchain/install/sysroots").join(&triple.to_string());
    let bin_dir = Path::new("toolchain/install/bin");
    let build_dir = Path::new("toolchain/build/crt").join(&triple.to_string());
    let llvm_cmake_dir = Path::new("toolchain/install/lib/cmake/llvm").canonicalize()?;
    let llvm_config = bin_dir.join("llvm-config");

    std::fs::create_dir_all(&install_dir)?;
    let _install_dir = install_dir.canonicalize()?;

    std::fs::create_dir_all(&bin_dir)?;
    let bin_dir = bin_dir.canonicalize()?;

    let _ = fs_extra::dir::remove(&build_dir);

    std::fs::create_dir_all(&build_dir)?;
    let build_dir = build_dir.canonicalize()?;

    let mut cfg = cmake::Config::new(&compiler_rt_dir);
    cfg.profile("Release");
    cfg.define("CMAKE_C_COMPILER_TARGET", triple.to_string());
    cfg.define("CMAKE_CXX_COMPILER_TARGET", triple.to_string());

    cfg.define("COMPILER_RT_BUILD_BUILTINS", "ON");
    cfg.define("COMPILER_RT_BUILD_CRT", "ON");
    cfg.define("COMPILER_RT_BUILD_SANITIZERS", "OFF");
    cfg.define("COMPILER_RT_BAREMETAL_BUILD", "ON");
    cfg.define("BUILD_SHARED_LIBS", "ON");
    cfg.cflag("-nostdlib");
    cfg.cxxflag("-nostdlib");
    cfg.cflag("-fno-stack-protector");
    cfg.asmflag("-target");
    cfg.asmflag(&triple.to_string());
    cfg.asmflag("-nostdinc");

    cfg.define("COMPILER_RT_BUILD_LIBFUZZER", "OFF");
    cfg.define("COMPILER_RT_BUILD_PROFILE", "OFF");
    cfg.define("COMPILER_RT_BUILD_XRAY", "OFF");
    cfg.define("COMPILER_RT_DEFAULT_TARGET_ONLY", "ON");
    cfg.define("COMPILER_RT_USE_LIBCXX", "OFF");
    cfg.define("LLVM_CONFIG_PATH", &llvm_config);

    cfg.define("COMPILER_RT_USE_BUILTINS_LIBRARY", "ON");

    setup_cmake(&mut cfg, None)?;
    setup_cmake_twizzler(&mut cfg, triple, vec!["-nostdlib".to_string()])?;

    cfg.define("LLVM_CMAKE_DIR", &llvm_cmake_dir)
        .define("LLVM_DIR", &llvm_cmake_dir)
        .define("LLVM_INCLUDE_TESTS", "OFF");

    cfg.out_dir(&build_dir);

    let build_target = format!("libclang_rt.builtins-{}.a", triple.arch);
    cfg.build_target(&build_target);
    cfg.build();

    let src = build_dir.join("build/lib/twizzler").join(&build_target);
    let mut dest = bin_dir.clone();
    dest.pop();
    let dest = dest.join("lib/clang/21/lib").join(&triple.to_string());
    std::fs::create_dir_all(&dest)?;

    let dest = dest.join("libclang_rt.builtins.a");
    std::fs::copy(src, dest)?;

    Ok(())
}

fn unhashed_basename(obj: &Path) -> &str {
    let basename = obj.file_stem().unwrap().to_str().expect("UTF-8 file name");
    basename.split_once('-').unwrap().1
}
