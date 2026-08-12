use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

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
    let sysroot = Path::new("toolchain/install/sysroots").join(triple.to_string());

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

    // These are provided by the initrd.
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
