use std::sync::{LazyLock, Mutex};

use fatfs::{FatType, FileSystem, FormatVolumeOptions};

use super::disk::Disk;

pub const PAGE_SIZE: usize = 4096;
pub const SECTOR_SIZE: usize = 512;
fn format(disk: &mut Disk) {
    let options = FormatVolumeOptions::new()
        .bytes_per_sector(SECTOR_SIZE as u16)
        .bytes_per_cluster(PAGE_SIZE as u32)
        .fat_type(FatType::Fat32);
    fatfs::format_volume(disk, options).unwrap();
}

pub static DISK: LazyLock<Disk> = LazyLock::new(|| Disk::new().unwrap());

pub static FS: LazyLock<Mutex<FileSystem<Disk>>> = LazyLock::new(|| {
    let disk = DISK.clone();
    let fs_options = fatfs::FsOptions::new().update_accessed_date(false);
    let fs = FileSystem::new(disk, fs_options);
    if let Ok(fs) = fs {
        return Mutex::new(fs);
    }
    drop(fs);
    let mut disk = Disk::new().unwrap();
    format(&mut disk);
    let fs =
        FileSystem::new(disk, fs_options).expect("disk should be formatted now so no more errors.");
    Mutex::new(fs)
});
