use std::{
    io::{self, Error, ErrorKind},
    sync::Arc,
};

use fatfs::{IoBase, Read, Seek, SeekFrom, Write};
use twizzler_async::block_on;

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
    pub fn new() -> Result<Disk, ()> {
        Ok(Disk {
            ctrl: block_on(init_nvme()),
            pos: 0,
        })
    }
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
            block_on(self.ctrl.read_page(lba as u64, &mut read_buffer, 0));

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
                self.seek(SeekFrom::Start(temp_pos & !PAGE_MASK as u64));
                self.read_exact(&mut write_buffer)?;
                self.seek(SeekFrom::Start(temp_pos));
            }

            write_buffer[left..right].copy_from_slice(&buf[bytes_read..bytes_read + right - left]);
            bytes_read += right - left;

            self.pos += right - left;

            block_on(self.ctrl.write_page(lba as u64, &mut write_buffer, 0));
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
