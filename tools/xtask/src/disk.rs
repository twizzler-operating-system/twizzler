use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    process::Command,
};

use ext4_lwext4::OpenFlags;

use crate::{triple::Triple, DiskCmd, DiskImageOptions};

const DISK_IMAGE_SIZE: u64 = 1024 * 1024 * 1024 * 100; // 100 GB

pub fn create_fresh_disk_image(triple: &Triple) -> anyhow::Result<()> {
    let path = format!("target/disk-{}.img", triple);
    println!("Creating disk image for {}", triple);
    if let Ok(f) = OpenOptions::new().write(true).create(true).open(&path) {
        f.set_len(DISK_IMAGE_SIZE).unwrap();
    }

    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            std::env::var("PATH").unwrap(),
            "/opt/homebrew/opt/e2fsprogs/sbin/"
        ),
    );
    std::env::set_var(
        "PATH",
        format!("{}:{}", std::env::var("PATH").unwrap(), "/usr/sbin/"),
    );
    let fs_type = "ext2";
    if !Command::new("mke2fs")
        .arg("-b")
        .arg("4096")
        .arg("-qF")
        .arg("-E")
        .arg("test_fs,lazy_itable_init=0,lazy_journal_init=0")
        .arg("-t")
        .arg(fs_type)
        .arg(&path)
        .arg((DISK_IMAGE_SIZE / 4096).to_string())
        .status()
        .expect("failed to create disk image")
        .success()
    {
        panic!("failed to run mke2fs on {}", path);
    }

    copy_sysroot(triple, true)?;

    Ok(())
}

pub fn copy_sysroot(triple: &Triple, force: bool) -> anyhow::Result<()> {
    let sysroot = Path::new("toolchain/install/sysroots").join(triple.to_string());
    let path = format!("target/disk-{}.img", triple);

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

    let image_time = std::fs::metadata(&path)?.modified()?;
    println!("Copying sysroot to disk image for {}", triple,);

    if image_time > latest_time && !force {
        println!("Disk image is up to date, skipping copy.");
        return Ok(());
    }

    let device = ext4_lwext4::FileBlockDevice::open(path)?;
    let ext4 = ext4_lwext4::Ext4Fs::mount(device, false)?;

    let mut completed_files = 0;
    let mut completed_bytes = 0;

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
                entry.file_name().display()
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

    Ok(())
}

pub fn do_disk_image(opts: DiskImageOptions) -> anyhow::Result<()> {
    match opts.cmd {
        DiskCmd::Reset => create_fresh_disk_image(&opts.config.twz_triple()),
        DiskCmd::Setup => copy_sysroot(&opts.config.twz_triple(), opts.force),
    }
}
