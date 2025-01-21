use std::{
    io,
    io::{Error, ErrorKind},
    sync::Arc,
};

use layout::{
    collections::raw::RawBytes, io::SeekFrom, Encode, Read, Seek, SourcedDynamic, Write, IO,
};
use lethe_gadget_fat::schema::{self, FATEntry, Superblock};
use twizzler_async::block_on;

use crate::nvme::{init_nvme, NvmeController};

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

pub fn setup(data: &mut Disk) {
    let super_block = Superblock {
        magic: 0,
        block_size: 0x200,
        block_count: 0x1000,
    };

    let fat = vec![FATEntry::None; super_block.block_count as usize].into_boxed_slice();

    let mut fs = schema::FileSystem {
        super_block: super_block.clone(),
        fat,
        super_block_cp: super_block,
        obj_lookup: vec![FATEntry::None; 3].into_boxed_slice(),
        rest: RawBytes,
    };

    let fs_size = fs.sourced_size();
    let reserved_blocks = fs_size / fs.super_block.block_size as u64
        + (fs_size % fs.super_block.block_size as u64).min(1);

    fs.fat[0] = FATEntry::Block(reserved_blocks);
    fs.fat[1..reserved_blocks as usize].fill(FATEntry::Reserved);
    for i in reserved_blocks..fs.super_block.block_count - 1 {
        fs.fat[i as usize] = FATEntry::Block(i + 1);
    }
    fs.fat[fs.super_block.block_count as usize - 1] = FATEntry::None;

    fs.encode(data).unwrap();
}

const DISK_SIZE: usize = 0x1000000;
const PAGE_SIZE: usize = 4096;
const SECTOR_SIZE: usize = 512;
const PAGE_MASK: usize = 0xFFF;
const LBA_COUNT: usize = DISK_SIZE / SECTOR_SIZE;

impl Read for Disk {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
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
            block_on(self.ctrl.read_page(lba as u64, &mut read_buffer, 0)).unwrap();

            let bytes_to_read = right - left;
            buf[bytes_written..bytes_written + bytes_to_read]
                .copy_from_slice(&read_buffer[left..right]);

            bytes_written += bytes_to_read;
            self.pos += bytes_to_read;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_written)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.read(buf)?;
        Ok(())
    }
}

impl Write for Disk {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
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

            block_on(self.ctrl.write_page(lba as u64, &mut write_buffer, 0)).unwrap();
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_read)
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        self.write(buf)?;

        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Seek for Disk {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x as i64,
            SeekFrom::End(x) => (DISK_SIZE as i64) - x,
            SeekFrom::Current(x) => (self.pos as i64) + x,
        };

        if new_pos > DISK_SIZE.try_into().unwrap() || new_pos < 0 {
            Err(Error::new(ErrorKind::AddrInUse, "oh no!"))
        } else {
            self.pos = new_pos as usize;
            Ok(self.pos.try_into().unwrap())
        }
    }
}

impl IO for Disk {
    type Error = io::Error;
}
