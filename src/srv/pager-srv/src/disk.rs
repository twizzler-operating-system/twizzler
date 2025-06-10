use std::{
    collections::HashMap,
    i64,
    io::{Error, ErrorKind, Read, Seek, SeekFrom, Write},
    sync::Arc,
    u32, u64,
};

use async_executor::Executor;
use object_store::PagingImp;
use twizzler_abi::pager::PhysRange;
use twizzler_driver::dma::{PhysAddr, PhysInfo};

use crate::{
    nvme::{init_nvme, NvmeController},
    physrw, EXECUTOR, PAGER_CTX,
};

const PAGE_SIZE: usize = 0x1000;
const SECTOR_SIZE: usize = 512;

pub struct DiskPageRequest {
    phys_addr_list: Vec<PhysInfo>,
    ctrl: Arc<NvmeController>,
}

impl core::fmt::Debug for DiskPageRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskPageRequest")
            .field("phys_addr_list", &self.phys_addr_list)
            .finish_non_exhaustive()
    }
}

impl PagingImp for DiskPageRequest {
    type PhysAddr = PhysInfo;

    fn fill_from_buffer(&self, buf: &[u8]) {
        let pager = PAGER_CTX.get().unwrap();
        for (buf, pa) in buf
            .chunks(Self::page_size())
            .zip(self.phys_addr_list.iter())
        {
            let pr = PhysRange::new(
                pa.addr().into(),
                u64::from(pa.addr()) + Self::page_size() as u64,
            );
            async_io::block_on(EXECUTOR.get().unwrap().run(physrw::fill_physical_pages(
                &pager.sender,
                buf,
                pr,
            )))
            .unwrap();
        }
    }

    fn read_to_buffer(&self, buf: &mut [u8]) {
        let pager = PAGER_CTX.get().unwrap();
        for (buf, pa) in buf
            .chunks_mut(Self::page_size())
            .zip(self.phys_addr_list.iter())
        {
            let pr = PhysRange::new(
                pa.addr().into(),
                u64::from(pa.addr()) + Self::page_size() as u64,
            );
            async_io::block_on(EXECUTOR.get().unwrap().run(physrw::read_physical_pages(
                &pager.sender,
                buf,
                pr,
            )))
            .unwrap();
        }
    }

    fn phys_addrs(&self) -> impl Iterator<Item = &'_ Self::PhysAddr> {
        self.phys_addr_list.iter()
    }

    fn page_in(&self, disk_pages: impl Iterator<Item = Option<u64>>) -> std::io::Result<usize> {
        let mut pairs = disk_pages
            .zip(self.phys_addrs())
            .filter_map(|(x, y)| if let Some(x) = x { Some((x, y)) } else { None })
            .collect::<Vec<_>>();
        tracing::debug!("page-in: pairs: {:?}", pairs);
        pairs.sort_by_key(|p| p.0);
        let (dp, pp): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        let mut offset = 0;
        let runs = crate::helpers::consecutive_slices(&dp).map(|run| {
            let pair = (run, &pp[offset..(offset + run.len())]);
            offset += run.len();
            pair
        });
        let mut count = 0;
        for (dp, pp) in runs {
            tracing::debug!("  seqread: {:?} => {:?}", dp, pp);
            let len = self.ctrl.sequential_read::<PAGE_SIZE>(dp[0], pp)?;
            assert_eq!(len, pp.len());
            count += len;
        }
        Ok(count)
    }

    fn page_out(&self, disk_pages: impl Iterator<Item = Option<u64>>) -> std::io::Result<usize> {
        let mut pairs = disk_pages
            .zip(self.phys_addrs())
            .filter_map(|(x, y)| if let Some(x) = x { Some((x, y)) } else { None })
            .collect::<Vec<_>>();
        tracing::debug!("page-out: pairs: {:?}", pairs);
        pairs.sort_by_key(|p| p.0);
        let (dp, pp): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        let mut offset = 0;
        let runs = crate::helpers::consecutive_slices(&dp).map(|run| {
            let pair = (run, &pp[offset..(offset + run.len())]);
            offset += run.len();
            pair
        });
        let mut count = 0;
        for (dp, mut pp) in runs {
            let mut offset = 0;
            while pp.len() > 0 {
                tracing::trace!("  seqwrite: {:?} => {} pages", dp, pp.len());
                let len = self
                    .ctrl
                    .sequential_write::<PAGE_SIZE>(dp[0] + offset as u64, pp)?;
                count += len;
                offset += len;
                pp = &pp[len..];
            }
        }
        Ok(count)
    }
}

#[allow(dead_code)]
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
        let ctrl = init_nvme().await.expect("failed to open nvme controller");
        tracing::info!("getting len");
        let len = ctrl.flash_len().await;
        let len = std::cmp::max(len, u32::MAX as usize / SECTOR_SIZE);
        tracing::info!("disk ready");
        Ok(Disk {
            ctrl,
            pos: 0,
            cache: HashMap::new(),
            len,
            ex,
        })
    }

    pub fn lba_count(&self) -> usize {
        self.len / SECTOR_SIZE
    }

    pub fn new_paging_request<P: PagingImp>(
        &self,
        pages: impl IntoIterator<Item = u64>,
    ) -> DiskPageRequest {
        DiskPageRequest {
            phys_addr_list: pages
                .into_iter()
                .map(|addr| PhysInfo::new(PhysAddr(addr)))
                .collect(),
            ctrl: self.ctrl.clone(),
        }
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
                self.ctrl
                    .blocking_read_page(lba as u64, &mut read_buffer, 0)?;
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
            self.ctrl
                .blocking_write_page(lba as u64, &mut write_buffer, 0)?;
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
