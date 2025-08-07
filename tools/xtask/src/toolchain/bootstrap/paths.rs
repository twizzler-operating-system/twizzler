use std::path::PathBuf;

pub fn get_rust_stage2_std(host_triple: &str, target_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let dir = curdir
        //TODO: move this into install
        .join("toolchain/src/rust/build")
        .join(host_triple)
        .join("stage2-std")
        .join(target_triple)
        .join("release");
    Ok(dir)
}

pub fn get_llvm_native_runtime(target_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let arch = target_triple.split("-").next().unwrap();
    let archive_name = format!("libclang_rt.builtins-{}.a", arch);
    //TODO: move this into install
    let dir = curdir
        .join("toolchain/src/rust/build")
        .join(target_triple)
        .join("native/sanitizers/build/lib/twizzler")
        .join(archive_name);
    Ok(dir)
}

pub fn get_llvm_native_runtime_install(target_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let archive_name = "libclang_rt.builtins.a";
    let dir = curdir
        .join("toolchain/install/lib/clang/20/lib")
        .join(target_triple)
        .join(archive_name);
    Ok(dir)
}

pub fn get_rust_lld(host_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let rustlib_bin = curdir
        .join("toolchain/src/rust/build")
        .join(host_triple)
        .join("stage1/lib/rustlib")
        .join(host_triple)
        .join("bin/rust-lld");
    Ok(rustlib_bin)
}
pub fn get_llvm_bin(host_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let llvm_bin = curdir
        //TODO: move this into install
        .join("toolchain/src/rust/build")
        .join(host_triple)
        .join("llvm/bin");
    Ok(llvm_bin)
}

pub fn get_lld_bin(host_triple: &str) -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let llvm_bin = curdir
        //TODO: move this into install
        .join("toolchain/src/rust/build")
        .join(host_triple)
        .join("lld/bin");
    Ok(llvm_bin)
}

pub fn get_compiler_rt_path() -> anyhow::Result<PathBuf> {
    let curdir = std::env::current_dir().unwrap();
    let compiler_rt = curdir.join("toolchain/src/rust/src/llvm-project/compiler-rt");

    Ok(compiler_rt)
}

pub fn get_builtin_headers() -> anyhow::Result<PathBuf> {
    //TODO: maybe we should throw up a warning to ask the user to switch to the proper directory?
    let curdir = std::env::current_dir().unwrap();
    let headers = curdir.join("toolchain/src/rust/build/host/llvm/lib/clang/20/include/");

    Ok(headers)
}
