use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use ext4_lwext4::{mkfs, OpenFlags};

use crate::{build::TwizzlerCompilation, triple::Triple, DiskCmd, DiskImageOptions};

const DISK_IMAGE_SIZE: u64 = 1024 * 1024 * 1024 * 100; // 100 GB

/// Where `copy_twizzler_build` stages the `#[test]` binaries. The guest reaches these as
/// `/pkg/twizzler/test`, via the `/pkg -> /ext/sysroot/pkg` symlink `init` sets up; keep this in
/// step with the first entry of `unittest`'s `SEARCH_DIRS`.
const TEST_DIR_ON_DISK: &str = "/sysroot/pkg/twizzler/test";

/// Where the ext4 data disk lives: the caller's `--disk-image`, or the shared default.
///
/// The default is shared by every invocation for a triple, including across profiles, which is why
/// writing it is serialized (see [`crate::imagelock`]). A caller that wants to build without
/// touching the build tree's copy passes its own path; it is created if absent and updated in
/// place if present.
pub fn image_path(triple: &Triple, over: Option<&Path>) -> PathBuf {
    over.map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(format!("target/disk-{}.img", triple)))
}

pub fn create_fresh_disk_image(triple: &Triple, path: &Path) -> anyhow::Result<()> {
    println!("Creating disk image for {} at {}", triple, path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Ok(f) = OpenOptions::new().write(true).create(true).open(&path) {
        f.set_len(DISK_IMAGE_SIZE).unwrap();
    }

    let device = ext4_lwext4::FileBlockDevice::open(path)?;
    let options = mkfs::MkfsOptions {
        ..Default::default()
    };
    mkfs(device, &options).unwrap();

    copy_sysroot(triple, path, true)?;

    Ok(())
}

pub fn copy_sysroot(triple: &Triple, path: &Path, force: bool) -> anyhow::Result<()> {
    let sysroot = crate::toolchain::get_sysroots_root()?.join(triple.to_string());

    let mut latest_time = std::time::UNIX_EPOCH;
    let mut total_bytes = 0;
    let mut total_files = 0;
    walkdir::WalkDir::new(&sysroot)
        .into_iter()
        .for_each(|entry| {
            let entry = entry.unwrap();
            total_files += 1;
            total_bytes += entry.metadata().unwrap().len();

            if entry.file_type().is_file() {
                if let Ok(metadata) = entry.metadata() {
                    if let Ok(modified) = metadata.modified() {
                        if modified > latest_time {
                            latest_time = modified;
                        }
                    }
                }
            }
        });

    if std::fs::exists(&path)? {
        let image_time = std::fs::metadata(&path)?.modified()?;

        if image_time > latest_time && !force {
            println!("Disk image sysroot is up to date, skipping sysroot copy.");
            return Ok(());
        }
    } else {
        return create_fresh_disk_image(triple, path);
    }

    println!("Copying sysroot to disk image for {}", triple,);

    let device = ext4_lwext4::FileBlockDevice::open(path)?;
    let ext4 = ext4_lwext4::Ext4Fs::mount(device, false)?;

    let mut completed_files = 0;
    let mut completed_bytes = 0;

    ext4.mkdir("/sysroot", 0755).unwrap();

    walkdir::WalkDir::new(&sysroot)
        .into_iter()
        .try_for_each(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();

            print!(
                "copying {:5}/{:5} MB ({:10}/{:10} files): {}                           \r",
                completed_bytes / (1024 * 1024),
                total_bytes / (1024 * 1024),
                completed_files,
                total_files,
                entry.file_name().display(),
            );
            std::io::stdout().flush().unwrap();

            let image_path = entry.path().strip_prefix(&sysroot).unwrap();
            if entry.file_type().is_file() {
                let mut dest = Path::new("/sysroot").to_path_buf();
                for comp in image_path.parent().unwrap().components() {
                    dest.push(comp);

                    ext4.mkdir(dest.to_str().unwrap(), 0o755).unwrap();
                }
                dest.push(image_path.file_name().unwrap());

                if ext4.exists(dest.to_str().unwrap()) {
                    ext4.remove(dest.to_str().unwrap()).unwrap();
                }

                let mut dest_file = ext4
                    .open(
                        dest.to_str().unwrap(),
                        OpenFlags::READ | OpenFlags::WRITE | OpenFlags::CREATE,
                    )
                    .unwrap();
                let mut src_file = File::open(entry.path())?;

                std::io::copy(&mut src_file, &mut dest_file).unwrap();
            } else if entry.file_type().is_dir() {
                let mut dest = Path::new("/sysroot").to_path_buf();
                for comp in image_path.components() {
                    dest.push(comp);
                    ext4.mkdir(dest.to_str().unwrap(), 0o755).unwrap();
                }
            } else if entry.file_type().is_symlink() {
                let target = std::fs::read_link(entry.path()).unwrap();
                let mut link = Path::new("/sysroot").to_path_buf();
                link.push(image_path);
                if ext4.exists(link.to_str().unwrap()) {
                    ext4.remove(link.to_str().unwrap()).unwrap();
                }
                ext4.symlink(target.to_str().unwrap(), link.to_str().unwrap())
                    .unwrap();
            }

            completed_bytes += metadata.len();
            completed_files += 1;

            Ok::<_, std::io::Error>(())
        })?;

    // Both are provided by the initrd at *runtime*, so the copies staged into the sysroot at
    // toolchain-install time are never loaded and can be arbitrarily old. Dropping them here keeps
    // a stale build from being what an on-target link resolves against; `copy_twizzler_build`
    // puts the freshly built libtwz_rt.so back, which is the only copy that should ever be linked.
    ext4.remove("/sysroot/lib/libtwz_rt.so").unwrap();
    ext4.remove("/sysroot/lib/libc.so").unwrap();
    ext4.mkdir("/sysroot/pkg", 0o755).unwrap();
    ext4.mkdir("/sysroot/etc", 0o755).unwrap();

    let _ = ext4.remove("/sysroot/etc/services");
    let mut file = ext4
        .open(
            "/sysroot/etc/services",
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE | OpenFlags::READ,
        )
        .unwrap();

    let host_file = File::open("/etc/services")?;
    std::io::copy(&mut &host_file, &mut file).unwrap();
    file.flush().unwrap();

    let _ = ext4.remove("/sysroot/etc/resolv.conf");
    let mut file = ext4
        .open(
            "/sysroot/etc/resolv.conf",
            OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE | OpenFlags::READ,
        )
        .unwrap();
    write!(file, "nameserver 8.8.8.8\n").unwrap();
    file.flush().unwrap();

    ext4.symlink("/pkg/ncurses/share/terminfo", "/sysroot/etc/terminfo")
        .unwrap();

    Ok(())
}

/// The names `uuhelper` answers to, read from its own manifest rather than duplicated here.
///
/// uuhelper is a multi-call binary: it picks the utility from the file stem of `argv[0]`, so each
/// name is a symlink to the one executable. The set is exactly the `feat_os_twizzler` feature
/// list, because that is what decides which `uu_*` crates are compiled in -- a name not in it
/// resolves to a binary that then fails to dispatch, which is a worse failure than the name
/// simply not existing.
///
/// Derived, not copied: a hardcoded list here drifted within minutes of being written, when
/// uuhelper gained 15 utilities in another session. Reading the manifest makes that class of
/// skew impossible rather than merely discouraged.
fn uuhelper_utils(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    let manifest_path = repo_root.join("src/bin/uuhelper/Cargo.toml");
    let manifest = cargo_toml::Manifest::from_path(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let feats = manifest
        .features
        .get("feat_os_twizzler")
        .ok_or_else(|| anyhow::anyhow!("uuhelper has no feat_os_twizzler feature"))?;
    Ok(feats.clone())
}

pub fn copy_twizzler_build(
    build: &TwizzlerCompilation,
    triple: &Triple,
    path: &Path,
) -> anyhow::Result<()> {
    copy_sysroot(triple, path, false)?;
    let device = ext4_lwext4::FileBlockDevice::open(path)?;
    let ext4 = ext4_lwext4::Ext4Fs::mount(device, false)?;

    println!("Copying Twizzler build to disk image for {}", triple,);
    for cd in build
        .borrow_user_compilation()
        .as_ref()
        .unwrap()
        .cdylibs
        .iter()
        .chain(
            build
                .borrow_user_compilation()
                .as_ref()
                .unwrap()
                .binaries
                .iter(),
        )
    {
        ext4.mkdir("/sysroot", 0o755).unwrap();
        ext4.mkdir("/sysroot/pkg", 0o755).unwrap();
        ext4.mkdir("/sysroot/pkg/twizzler", 0o755).unwrap();
        ext4.mkdir("/sysroot/pkg/twizzler/bin", 0o755).unwrap();
        ext4.mkdir("/sysroot/pkg/twizzler/lib", 0o755).unwrap();
        let mut dest = Path::new("/sysroot/pkg/twizzler").to_path_buf();

        if cd.path.extension().is_some_and(|x| x == "so") {
            dest.push("lib");
        } else {
            dest.push("bin");
        }

        dest.push(cd.path.file_name().unwrap());

        if ext4.exists(dest.to_str().unwrap()) {
            ext4.remove(dest.to_str().unwrap()).unwrap();
        }

        let mut dest_file = ext4
            .open(
                dest.to_str().unwrap(),
                OpenFlags::READ | OpenFlags::WRITE | OpenFlags::CREATE,
            )
            .unwrap();
        let mut src_file = File::open(&cd.path)?;

        std::io::copy(&mut src_file, &mut dest_file).unwrap();

        // Also install the runtime under its conventional name, so an on-target
        // `rustc`/`ld.lld` resolves -ltwz_rt from -L/sysroot/lib like any other library. Every
        // other `twz_rt_*` provider a Twizzler binary needs is already there; without this the
        // link fails with ~20 undefined symbols and no hint that the library exists at all.
        // Copied from the build output rather than left as the sysroot's staged copy: that one
        // is written at toolchain-install time and goes stale as soon as the runtime is rebuilt.
        if cd.path.file_name().is_some_and(|n| n == "libtwz_rt.so") {
            let rt_dest = "/sysroot/lib/libtwz_rt.so";
            if ext4.exists(rt_dest) {
                ext4.remove(rt_dest).unwrap();
            }
            let mut rt_file = ext4
                .open(
                    rt_dest,
                    OpenFlags::READ | OpenFlags::WRITE | OpenFlags::CREATE,
                )
                .unwrap();
            let mut rt_src = File::open(&cd.path)?;
            std::io::copy(&mut rt_src, &mut rt_file).unwrap();
        }
    }

    // libc.so gets the same treatment as libtwz_rt.so above, for the same reason: `copy_sysroot`
    // deletes the staged copy (a stale one is a link-time trap -- an on-target `-lc` against an
    // old libc.so mismatches the initrd's runtime copy), so something must put a current one
    // back. Without it, lld silently satisfies `-lc` from libc.a, and a statically-linked mlibc
    // never receives its entry stack -- every such binary runs with libc state (environ
    // included) that nothing initialized. Freshness source is the toolchain sysroot, which is
    // also where the initrd's runtime copy comes from.
    let libc_src = crate::toolchain::get_sysroots_root()?
        .join(triple.to_string())
        .join("lib/libc.so");
    let libc_dest = "/sysroot/lib/libc.so";
    if ext4.exists(libc_dest) {
        ext4.remove(libc_dest).unwrap();
    }
    let mut libc_dest_file = ext4
        .open(
            libc_dest,
            OpenFlags::READ | OpenFlags::WRITE | OpenFlags::CREATE,
        )
        .unwrap();
    let mut libc_src_file = File::open(&libc_src)?;
    std::io::copy(&mut libc_src_file, &mut libc_dest_file).unwrap();
    drop(libc_dest_file);

    // uuhelper's aliases are made here, at image build time, rather than by init on every boot.
    // They describe the contents of the image, so they belong to whatever writes the image: as
    // runtime work they cost a naming call per utility per boot and, being naming-server nodes
    // rather than ext4 entries, they did not survive into the image at all -- every boot rebuilt
    // state that was already known when the disk was made.
    //
    // Guarded on uuhelper actually being staged: a link to a binary that is not there resolves
    // to a name that then fails to spawn, which is a worse failure than the name not existing.
    if ext4.exists("/sysroot/pkg/twizzler/bin/uuhelper") {
        for util in uuhelper_utils(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .as_path(),
        )? {
            let link = format!("/sysroot/pkg/twizzler/bin/{}", util);
            if ext4.exists(&link) {
                ext4.remove(&link).unwrap();
            }
            // The target is a *guest* path, not an image path: it is resolved on Twizzler, where
            // the image's /sysroot is reached as /pkg. Same convention as the terminfo link above.
            ext4.symlink("/pkg/twizzler/bin/uuhelper", &link).unwrap();
        }
    }

    // The `#[test]` binaries go on the disk rather than into the initrd. The initrd is read whole
    // through UEFI block I/O at boot (~90MB/s) whether or not a test ever runs; the disk is
    // demand-paged, so only the binaries actually spawned cost anything. Their own directory keeps
    // them out of PATH and out of the way of the real programs in bin/.
    if let Some(test_comp) = build.borrow_user_test_compilation().as_ref() {
        for dir in [
            "/sysroot",
            "/sysroot/pkg",
            "/sysroot/pkg/twizzler",
            TEST_DIR_ON_DISK,
        ] {
            ext4.mkdir(dir, 0o755).unwrap();
        }
        for test in test_comp.tests.iter() {
            let dest = Path::new(TEST_DIR_ON_DISK).join(test.path.file_name().unwrap());
            let dest = dest.to_str().unwrap();

            if ext4.exists(dest) {
                ext4.remove(dest).unwrap();
            }

            let mut dest_file = ext4
                .open(dest, OpenFlags::READ | OpenFlags::WRITE | OpenFlags::CREATE)
                .unwrap();
            let mut src_file = File::open(&test.path)?;

            std::io::copy(&mut src_file, &mut dest_file).unwrap();
        }
    }

    Ok(())
}

pub fn do_disk_image(opts: DiskImageOptions) -> anyhow::Result<()> {
    let triple = opts.config.twz_triple();
    let path = image_path(&triple, opts.disk_image.as_deref());
    let _image_lock = crate::imagelock::image_lock(&path)?;
    match opts.cmd {
        DiskCmd::Reset => create_fresh_disk_image(&triple, &path),
        DiskCmd::Setup => copy_sysroot(&triple, &path, opts.force),
    }
}
