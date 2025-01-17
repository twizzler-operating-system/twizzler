use std::sync::{LazyLock, Mutex};

use fatfs::{FatType, FileSystem, FormatVolumeOptions};

use super::disk::Disk;

pub const PAGE_SIZE: usize = 4096;
pub const SECTOR_SIZE: usize = 512;
pub(crate) fn format(disk: &mut Disk) {
    let options = FormatVolumeOptions::new()
        .bytes_per_sector(SECTOR_SIZE as u16)
        .bytes_per_cluster(PAGE_SIZE as u32)
        .fat_type(FatType::Fat32);
    fatfs::format_volume(disk, options).unwrap();
}
