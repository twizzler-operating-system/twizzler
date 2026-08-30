use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use super::{generate_tag, get_installed_toolchains};

/// A toolchain pinned explicitly for this invocation, overriding the one the current submodule
/// pointers imply.
///
/// The computed tag is a function of HEAD's submodule OIDs, so committing a pointer bump silently
/// redirects every path below at a toolchain nobody has built yet -- and a bootstrap in progress
/// repoints `toolchain/install` at its own half-written output. Pinning decouples both.
static TOOLCHAIN_OVERRIDE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Resolve `--toolchain`/`TWIZZLER_TOOLCHAIN` once, before any path is derived.
///
/// An unresolvable spec is a hard error rather than a fallback to the computed tag: a pin that
/// silently does not pin would build against the wrong compiler and look like it worked.
pub fn set_toolchain_override(spec: Option<String>) -> anyhow::Result<()> {
    let spec = spec
        .or_else(|| std::env::var("TWIZZLER_TOOLCHAIN").ok())
        .filter(|s| !s.is_empty());

    let resolved = match spec {
        None => None,
        Some(spec) => Some(resolve_toolchain_spec(&spec)?),
    };

    if let Some(path) = &resolved {
        // Always report where the name actually landed. A tag is only a directory name, and a
        // rename or symlink can divorce it from its contents -- `toolchain_1a107ff-96d91a4-4dc166f`
        // was a symlink to `toolchain_2e87390-5d603b4-8511b4b` on 2026-08-30, so a pin that
        // resolved happily selected three commits it did not contain.
        match path.canonicalize() {
            Ok(real) if real.file_name() != path.file_name() => {
                eprintln!("=== pinned toolchain: {}", path.display());
                eprintln!(
                    "=== WARNING: {} is a link to {} -- its name asserts commits it does not contain",
                    path.display(),
                    real.display()
                );
            }
            _ => eprintln!("=== pinned toolchain: {}", path.display()),
        }
    }

    let _ = TOOLCHAIN_OVERRIDE.set(resolved);
    Ok(())
}

/// Accepts a full tag (`toolchain_a-b-c`), a bare hash triple (`a-b-c`), or a path.
fn resolve_toolchain_spec(spec: &str) -> anyhow::Result<PathBuf> {
    let candidates: Vec<PathBuf> = if spec.contains('/') {
        vec![PathBuf::from(spec)]
    } else {
        vec![
            Path::new("toolchain").join(spec),
            Path::new("toolchain").join(format!("toolchain_{spec}")),
        ]
    };

    for candidate in &candidates {
        if candidate.join("bin/rustc").exists() {
            return Ok(candidate.clone());
        }
    }

    let installed = get_installed_toolchains().unwrap_or_default();
    let installed = if installed.is_empty() {
        "  (none)".to_string()
    } else {
        installed
            .iter()
            .map(|tc| format!("  {tc}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let looked = candidates
        .iter()
        .map(|c| c.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    anyhow::bail!(
        "no toolchain matching `{spec}` has a bin/rustc (looked at: {looked})\ninstalled toolchains:\n{installed}"
    )
}

fn toolchain_override() -> Option<&'static PathBuf> {
    TOOLCHAIN_OVERRIDE.get().and_then(|o| o.as_ref())
}

pub fn get_toolchain_path() -> anyhow::Result<PathBuf> {
    if let Some(pinned) = toolchain_override() {
        return Ok(pinned.clone());
    }

    let mut tc_path = PathBuf::from("toolchain");
    let tag = generate_tag()?;
    tc_path.push(tag);

    Ok(tc_path)
}

/// The sysroots directory of the toolchain in use.
///
/// Prefer this over `toolchain/install/sysroots`: `install` is a symlink a running bootstrap
/// repoints at its own output, so that literal path can read -- or write -- a half-built toolchain.
pub fn get_sysroots_root() -> anyhow::Result<PathBuf> {
    Ok(get_toolchain_path()?.join("sysroots"))
}

pub fn get_rustc_path() -> anyhow::Result<PathBuf> {
    let mut rustc_path = get_toolchain_path()?;

    rustc_path.push("bin/rustc");

    Ok(rustc_path)
}

pub fn get_rustdoc_path() -> anyhow::Result<PathBuf> {
    let mut rustdoc_path = get_toolchain_path()?;

    rustdoc_path.push("bin/rustdoc");
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

pub fn get_rustlib_lib(host_triple: &str) -> anyhow::Result<PathBuf> {
    let rustlib_bin = get_toolchain_path()?
        .join("lib/rustlib")
        .join(host_triple)
        .join("lib");
    Ok(rustlib_bin)
}

pub fn get_rust_stage2_std(host_triple: &str, target_triple: &str) -> anyhow::Result<PathBuf> {
    let dir = get_toolchain_path()?
        .join("rust/build")
        .join(host_triple)
        .join("stage1-std")
        .join(target_triple)
        .join("release");

    Ok(dir)
}

pub fn get_builtin_headers() -> anyhow::Result<PathBuf> {
    let headers = get_toolchain_path()?.join("lib/clang/21/include/");

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
    Ok(tc_path.canonicalize()?)
}
