use std::path::PathBuf;

use super::generate_tag;

pub fn get_toolchain_path() -> anyhow::Result<PathBuf> {
    let mut tc_path = PathBuf::from("toolchain");
    let tag = generate_tag()?;
    tc_path.push(tag);

    Ok(tc_path)
}

pub fn get_rustc_path() -> anyhow::Result<PathBuf> {
    let mut rustc_path = get_toolchain_path()?;

    rustc_path.push("bin/rustc");

    Ok(rustc_path)
}

pub fn get_rustdoc_path() -> anyhow::Result<PathBuf> {
    let mut rustdoc_path = get_toolchain_path()?;

    rustdoc_path.push("/bin/rustdoc");
    Ok(rustdoc_path)
}

pub fn get_bin_path() -> anyhow::Result<PathBuf> {
    let mut toolchain_bins = get_toolchain_path()?;
    toolchain_bins.push("bin");
    Ok(toolchain_bins)
}

pub fn clear_rustflags() {
    std::env::remove_var("RUSTFLAGS");
    std::env::remove_var("CARGO_TARGET_DIR");
}

pub fn get_lld_bin(host_triple: &str) -> anyhow::Result<PathBuf> {
    let llvm_bin = get_toolchain_path()?
        .join("rust/build")
        .join(host_triple)
        .join("lld/bin");
    Ok(llvm_bin)
}

pub fn get_rustlib_bin(host_triple: &str) -> anyhow::Result<PathBuf> {
    let rustlib_bin = get_toolchain_path()?
        .join("lib/rustlib")
        .join(host_triple)
        .join("bin");
    Ok(rustlib_bin)
}

pub fn get_rustlib_lib(host_triple: &str) -> anyhow::Result<PathBuf> {
    let rustlib_bin = get_toolchain_path()?
        .join("lib/rustlib")
        .join(host_triple)
        .join("lib");
    Ok(rustlib_bin)
}

pub fn get_compiler_rt_path() -> anyhow::Result<PathBuf> {
    let compiler_rt = get_toolchain_path()?.join("rust/src/llvm-project/compiler-rt");

    Ok(compiler_rt)
}

pub fn get_rust_lld(host_triple: &str) -> anyhow::Result<PathBuf> {
    let rustlib_bin = get_toolchain_path()?
        .join("rust/build")
        .join(host_triple)
        .join("stage1/lib/rustlib")
        .join(host_triple)
        .join("bin/rust-lld");
    Ok(rustlib_bin)
}

pub fn get_rust_stage2_std(host_triple: &str, target_triple: &str) -> anyhow::Result<PathBuf> {
    let dir = get_toolchain_path()?
        .join("rust/build")
        .join(host_triple)
        .join("stage2-std")
        .join(target_triple)
        .join("release");

    Ok(dir)
}

pub fn get_llvm_native_runtime_install(target_triple: &str) -> anyhow::Result<PathBuf> {
    let archive_name = "libclang_rt.builtins.a";
    let dir = get_toolchain_path()?
        .join("lib/clang/20/lib")
        .join(target_triple)
        .join(archive_name);
    Ok(dir)
}

pub fn get_builtin_headers() -> anyhow::Result<PathBuf> {
    let headers = get_toolchain_path()?.join("lib/clang/20/include/");

    Ok(headers)
}

pub fn get_python_path() -> anyhow::Result<PathBuf> {
    let mut python_path = get_toolchain_path()?;
    python_path.push("python");

    Ok(python_path)
}

pub fn get_sysroots_path(target_triple: &str) -> anyhow::Result<PathBuf> {
    let mut tc_path = get_toolchain_path()?;
    tc_path.push(format!("sysroots/{}/lib", target_triple));
    Ok(tc_path)
}
