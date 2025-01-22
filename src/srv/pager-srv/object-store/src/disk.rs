use std::{
    io::{Error, ErrorKind},
    sync::{Arc, Mutex, OnceLock},
};

use async_executor::Executor;
use async_io::block_on;
use fatfs::{FileSystem, IoBase, Read, Seek, SeekFrom};

use crate::{
    fs::{PAGE_SIZE, SECTOR_SIZE},
    nvme::{init_nvme, NvmeController},
};

const DISK_SIZE: usize = 0x40000000;
const PAGE_MASK: usize = 0xFFF;
const LBA_COUNT: usize = DISK_SIZE / SECTOR_SIZE;

#[derive(Clone)]
pub struct Disk {
    ctrl: Arc<NvmeController>,
    pub pos: usize,
}

impl Disk {
    pub fn new(ex: &'static Executor<'static>) -> Result<(Disk, Arc<NvmeController>), ()> {
        let ctrl = block_on(init_nvme(ex));
        Ok((
            Disk {
                ctrl: ctrl.clone(),
                pos: 0,
            },
            ctrl,
        ))
    }
}

pub static DISK: OnceLock<Disk> = OnceLock::new();
pub static FS: OnceLock<Mutex<FileSystem<Disk>>> = OnceLock::new();
pub static EXECUTOR: OnceLock<&'static Executor<'static>> = OnceLock::new();
pub static NVME: OnceLock<Arc<NvmeController>> = OnceLock::new();

pub fn init(ex: &'static Executor<'static>) {
    let (disk, fs, nvme) = do_init(ex);
    let _ = DISK.set(disk);
    let _ = FS.set(fs);
    let _ = NVME.set(nvme);
    let _ = EXECUTOR.set(ex);
}

fn do_init(ex: &'static Executor<'static>) -> (Disk, Mutex<FileSystem<Disk>>, Arc<NvmeController>) {
    let (disk, nvme) = Disk::new(ex).unwrap();
    let fs_options = fatfs::FsOptions::new().update_accessed_date(false);
    let fs = FileSystem::new(disk.clone(), fs_options);
    if let Ok(fs) = fs {
        return (disk, Mutex::new(fs), nvme);
    }
    drop(fs);
    let (mut disk, nvme) = Disk::new(ex).unwrap();
    super::fs::format(&mut disk);
    let fs = FileSystem::new(disk.clone(), fs_options)
        .expect("disk should be formatted now so no more errors.");
    (disk, Mutex::new(fs), nvme)
}

impl IoBase for Disk {
    type Error = std::io::Error;
}

impl fatfs::Read for Disk {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let mut lba = (self.pos / PAGE_SIZE) * 8;
        let mut bytes_written: usize = 0;
        let mut read_buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

        while bytes_written != buf.len() {
            if lba >= LBA_COUNT {
                break;
            }

            let left = self.pos % PAGE_SIZE;
            let right = if left + buf.len() - bytes_written > PAGE_SIZE {
                PAGE_SIZE
            } else {
                left + buf.len() - bytes_written
            }; // If I want to write more than the boundary of a page
            block_on(self.ctrl.read_page(lba as u64, &mut read_buffer, 0))
                .map_err(|_| ErrorKind::Other)?;

            let bytes_to_read = right - left;
            buf[bytes_written..bytes_written + bytes_to_read]
                .copy_from_slice(&read_buffer[left..right]);

            bytes_written += bytes_to_read;
            self.pos += bytes_to_read;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_written)
    }
}

impl fatfs::Write for Disk {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        let mut lba = (self.pos / PAGE_SIZE) * 8;
        let mut bytes_read = 0;
        let mut write_buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

        while bytes_read != buf.len() {
            if lba >= LBA_COUNT {
                break;
            }

            let left = self.pos % PAGE_SIZE;
            let right = if left + buf.len() - bytes_read > PAGE_SIZE {
                PAGE_SIZE
            } else {
                left + buf.len() - bytes_read
            };
            if right - left != PAGE_SIZE {
                let temp_pos: u64 = self.pos.try_into().unwrap();
                self.seek(SeekFrom::Start(temp_pos & !PAGE_MASK as u64))?;
                self.read_exact(&mut write_buffer)?;
                self.seek(SeekFrom::Start(temp_pos))?;
            }

            write_buffer[left..right].copy_from_slice(&buf[bytes_read..bytes_read + right - left]);
            bytes_read += right - left;

            self.pos += right - left;

            block_on(self.ctrl.write_page(lba as u64, &mut write_buffer, 0))
                .map_err(|_| ErrorKind::Other)?;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_read)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl fatfs::Seek for Disk {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (DISK_SIZE as i64) - x,
            SeekFrom::Current(x) => (self.pos as i64) + x,
        };
        if new_pos > DISK_SIZE.try_into().unwrap() || new_pos < 0 {
            println!("HERE!");
            Err(Error::new(ErrorKind::AddrInUse, "oh no!"))
        } else {
            self.pos = new_pos as usize;
            Ok(self.pos.try_into().unwrap())
        }
    }
}
