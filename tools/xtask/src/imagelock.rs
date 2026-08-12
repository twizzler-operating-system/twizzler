//! Cross-invocation serialization for the disk image and the shared build-tree staging.
//!
//! Two xtask invocations can be in flight at once and routinely are: several `many.py` sweeps, or a
//! sweep plus a developer's own `start-qemu`. They compile into per-profile directories and cargo
//! locks those, but [`crate::disk`] writes an ext4 image and [`crate::image`] stages the initrd and
//! boot image under `target/dynamic`, and neither is covered by anything cargo holds. A sweep's
//! build phase serializes against other sweeps per profile, which does not help: release and debug
//! builds hold different sweep locks and then mount the same image at the same time.
//!
//! The observed result was not a clean error. Two concurrent lwext4 mounts left one process
//! spinning at 100% cpu with no I/O for an hour inside the sysroot copy, and the image it had
//! half-written failed every later write with `Filesystem(5)` -- after which every guest booted
//! from it died with "failed to load library libtwz_rt.so".
//!
//! Two locks rather than one global lock, because `--disk-image` exists precisely so concurrent
//! sweeps can own private images: keying on the image path lets those run in parallel, while the
//! staging lock still covers what they genuinely share.
//!
//! Advisory `flock` (via cargo's own `FileLock`, already a dependency) rather than a lockfile we
//! create and remove: these processes get killed a lot, and a lock the kernel releases on exit
//! cannot go stale.

use std::path::{Path, PathBuf};

use cargo::util::{context::GlobalContext, FileLock, Filesystem};

use crate::{triple::Triple, Profile};

/// Serialize everyone writing one particular ext4 image.
///
/// Keyed on the image path, so `--disk-image` copies do not queue behind each other or behind the
/// default `target/disk-<triple>.img`.
pub fn image_lock(image: &Path) -> anyhow::Result<FileLock> {
    let dir = image.parent().unwrap_or(Path::new(".")).to_path_buf();
    let name = image
        .file_name()
        .map(|n| format!(".{}.xtask-lock", n.to_string_lossy()))
        .unwrap_or_else(|| ".xtask-image-lock".to_string());
    lock_at(dir, name, "the disk image")
}

/// Serialize the parts of `make-image` that write the build tree: the initrd staging directory and
/// the boot image, both keyed by triple and profile and shared by every invocation that builds
/// them, whatever disk image it is targeting.
pub fn staging_lock(triple: &Triple, profile: Profile) -> anyhow::Result<FileLock> {
    lock_at(
        PathBuf::from("target"),
        format!(".xtask-staging-{}-{}.lock", triple, profile),
        "the initrd and boot image staging",
    )
}

/// Not reentrant: `flock` is per open file description, so taking the same lock again inside the
/// scope of the first deadlocks the process against itself. Acquire at the top-level entry point
/// and let the helpers run under it. When both are needed, take `staging_lock` first.
fn lock_at(dir: PathBuf, name: String, what: &str) -> anyhow::Result<FileLock> {
    let gctx = GlobalContext::default()?;
    Filesystem::new(dir)
        .open_rw_exclusive_create(&name, &gctx, what)
        .map_err(|e| anyhow::anyhow!("failed to lock {}: {}", name, e))
}
