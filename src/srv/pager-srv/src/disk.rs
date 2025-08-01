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
    pub phys_addr_list: Vec<(u64, u64)>,
    pub ctrl: Arc<NvmeController>,
}

impl core::fmt::Debug for DiskPageRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskPageRequest")
            .field("phys_addr_list", &self.phys_addr_list)
            .finish_non_exhaustive()
    }
}

impl PagingImp for DiskPageRequest {
    fn fill_from_buffer(&self, buf: &[u8]) {
        let pager = PAGER_CTX.get().unwrap();
        for (buf, pa) in buf
            .chunks(Self::page_size())
            .zip(self.phys_addr_list.iter())
        {
            let pr = PhysRange::new(pa.0, pa.0 + pa.1);
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
            let pr = PhysRange::new(pa.0, pa.0 + pa.1);
            async_io::block_on(EXECUTOR.get().unwrap().run(physrw::read_physical_pages(
                &pager.sender,
                buf,
                pr,
            )))
            .unwrap();
        }
    }

    fn phys_addrs(&self) -> &[(u64, u64)] {
        self.phys_addr_list.as_slice()
    }

    fn page_in(&self, disk_pages: &[Option<u64>]) -> std::io::Result<usize> {
        let mut pairs = disk_pages
            .iter()
            .zip(
                self.phys_addrs()
                    .iter()
                    .map(|(p, _)| PhysInfo::new(PhysAddr(*p))),
            )
            .filter_map(|(x, y)| if let Some(x) = x { Some((*x, y)) } else { None })
            .collect::<Vec<_>>();
        //tracing::trace!("page-in: pairs: {:?}", pairs);
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
                //tracing::trace!("  seqread: {:?} => {:?}", dp, pp);
                let len = self
                    .ctrl
                    .sequential_read::<PAGE_SIZE>(dp[0] + offset as u64, pp)?;
                count += len;
                offset += len;
                pp = &pp[len..];
            }
        }
        Ok(count)
    }

    fn page_out(&self, disk_pages: &[Option<u64>]) -> std::io::Result<usize> {
        let mut pairs = disk_pages
            .iter()
            .zip(
                self.phys_addrs()
                    .iter()
                    .map(|(p, _)| PhysInfo::new(PhysAddr(*p))),
            )
            .filter_map(|(x, y)| if let Some(x) = x { Some((*x, y)) } else { None })
            .collect::<Vec<_>>();
        tracing::trace!("page-out: pairs: {:?}", pairs);
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

    fn phys_addrs_mut(&mut self) -> &mut Vec<(u64, u64)> {
        &mut self.phys_addr_list
    }
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct Disk {
    pub ctrl: Arc<NvmeController>,
    pub pos: usize,
    cache: HashMap<u64, Box<[u8; 4096]>>,
    pub len: usize,
    ex: &'static Executor<'static>,
}

impl Disk {
    pub async fn new(ex: &'static Executor<'static>) -> Result<Disk, ()> {
        let ctrl = init_nvme().await.expect("failed to open nvme controller");
        let len = ctrl.flash_len().await;
        let len = std::cmp::max(len, u32::MAX as usize / SECTOR_SIZE);
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

pub mod benches {
    use async_io::block_on;
    use rand::{rng, seq::SliceRandom};
    use twizzler_driver::dma::{PhysAddr, PhysInfo};

    use crate::{disk::PAGE_SIZE, PagerContext};

    extern crate test;

    pub fn do_bench<F: FnMut() -> usize>(mut f: F) -> String {
        let mut bytes = 0;
        let mut i = 0;
        let summary = test::bench::iter(&mut || {
            i += 1;
            bytes += f();
        });
        let ns_iter = std::cmp::max(summary.median as usize, 1);
        let mb_s = (bytes * 1000 / i) / ns_iter;
        let samples = test::bench::BenchSamples {
            ns_iter_summ: summary,
            mb_s,
        };
        test::bench::fmt_bench_samples(&samples)
    }

    #[allow(unused)]
    pub fn bench_disk(ctx: &'static PagerContext) {
        const NR_PAGES: usize = 128;
        let mut phys = (0..NR_PAGES)
            .map(|_| PhysInfo::new(PhysAddr(ctx.data.alloc_page().unwrap())))
            .collect::<Vec<_>>();
        // Check if the vector is sorted and each element is sequential
        let is_sequential = phys
            .windows(2)
            .all(|window| window[0].addr().0 + PAGE_SIZE as u64 == window[1].addr().0);

        let phys_size = phys.len() * PAGE_SIZE;
        let ctrl = block_on(crate::disk::init_nvme()).unwrap();
        if is_sequential {
            tracing::info!(
                "benching disk sequential read (with sequential memory): {} KB",
                phys_size / 1024
            );
            let result = do_bench(|| {
                let r = ctrl
                    .sequential_read::<PAGE_SIZE>(0, phys.as_slice())
                    .unwrap();
                assert_eq!(r, NR_PAGES);
                std::hint::black_box(r);
                phys_size
            });
            tracing::info!(" ==> {}", result);
        }

        phys.shuffle(&mut rng());

        tracing::info!(
            "benching disk sequential read (with random memory): {} KB",
            phys_size / 1024
        );
        let result = do_bench(&mut || {
            let r = ctrl
                .sequential_read::<PAGE_SIZE>(0, phys.as_slice())
                .unwrap();
            assert_eq!(r, NR_PAGES);
            std::hint::black_box(r);
            phys_size
        });
        tracing::info!(" ==> {}", result);
    }
}
