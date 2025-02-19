use std::{
    collections::HashMap,
    i64,
    io::{Error, ErrorKind, Read, Seek, SeekFrom, Write},
    sync::Arc,
    u32, u64,
};

use async_executor::Executor;
use async_io::block_on;

use crate::nvme::{init_nvme, NvmeController};

const PAGE_SIZE: usize = 0x1000;
const SECTOR_SIZE: usize = 512;

#[derive(Clone)]
pub struct Disk {
    ctrl: Arc<NvmeController>,
    pub pos: usize,
    cache: HashMap<u64, Box<[u8; 4096]>>,
    pub len: usize,
    ex: &'static Executor<'static>,
}

impl Disk {
    pub async fn new(ex: &'static Executor<'static>) -> Result<Disk, ()> {
        let ctrl = init_nvme(ex).await.expect("failed to open nvme controller");
        let len = todo!();
        //let len = ctrl.flash_len().await;
        let len = std::cmp::max(len, u32::MAX as usize / SECTOR_SIZE);
        Ok(Disk {
            ctrl,
            pos: 0,
            cache: HashMap::new(),
            len,
            ex,
        })
    }

    pub fn nvme(&self) -> &Arc<NvmeController> {
        &self.ctrl
    }

    pub fn lba_count(&self) -> usize {
        self.len / SECTOR_SIZE
    }
}

impl Read for Disk {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let mut lba = (self.pos / PAGE_SIZE) * 8;
        let mut bytes_written: usize = 0;
        let mut read_buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

        while bytes_written != buf.len() {
            if lba >= self.lba_count() {
                break;
            }

            let left = self.pos % PAGE_SIZE;
            let right = if left + buf.len() - bytes_written > PAGE_SIZE {
                PAGE_SIZE
            } else {
                left + buf.len() - bytes_written
            }; // If I want to write more than the boundary of a page

            if let Some(cached) = self.cache.get(&(lba as u64)) {
                read_buffer.copy_from_slice(&cached[0..4096]);
            } else {
                /*
                block_on(
                    self.ex
                        .run(self.ctrl.read_page(lba as u64, &mut read_buffer, 0)),
                )
                .map_err(|_| ErrorKind::Other)?;
                */
                todo!();
                self.cache.insert(lba as u64, Box::new(read_buffer));
            }

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

impl Write for Disk {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        let mut lba = (self.pos / PAGE_SIZE) * 8;
        let mut bytes_read = 0;
        let mut write_buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

        while bytes_read != buf.len() {
            if lba >= self.lba_count() {
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
                self.seek(SeekFrom::Start(temp_pos & !(PAGE_SIZE - 1) as u64))?;
                self.read_exact(&mut write_buffer)?;
                self.seek(SeekFrom::Start(temp_pos))?;
            }

            write_buffer[left..right].copy_from_slice(&buf[bytes_read..bytes_read + right - left]);
            bytes_read += right - left;

            self.pos += right - left;

            self.cache.insert(lba as u64, Box::new(write_buffer));
            /*
            block_on(
                self.ex
                    .run(self.ctrl.write_page(lba as u64, &mut write_buffer, 0)),
            )
            .map_err(|_| ErrorKind::Other)?;
            */
            todo!();
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_read)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl Seek for Disk {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Error> {
        let new_pos: i64 = match pos {
            SeekFrom::Start(x) => x.try_into().unwrap_or(i64::MAX),
            SeekFrom::End(x) => self.len.try_into().unwrap_or(i64::MAX).saturating_add(x),
            SeekFrom::Current(x) => self.pos.try_into().unwrap_or(i64::MAX).saturating_add(x),
        };
        if new_pos > self.len.try_into().unwrap_or(i64::MAX) || new_pos < 0 {
            Err(ErrorKind::UnexpectedEof.into())
        } else {
            self.pos = new_pos as usize;
            Ok(self.pos.try_into().unwrap_or(u64::MAX))
        }
    }
}
