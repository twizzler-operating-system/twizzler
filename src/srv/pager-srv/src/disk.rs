use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
    u32, u64,
};

use async_io::{block_on, Timer};
use object_store::{DevicePage, PagedDevice, PagedPhysMem, PhysRange, PosIo, MAYHEAP_LEN};
use twizzler::Result;
use twizzler_driver::dma::{PhysAddr, PhysInfo};

use crate::{
    helpers::PAGE,
    nvme::{init_nvme, NvmeController},
    threads::run_async,
    PAGER_CTX,
};

const PAGE_SIZE: usize = 0x1000;
pub const SECTOR_SIZE: usize = 512;

#[allow(dead_code)]
#[derive(Clone)]
pub struct Disk {
    pub ctrl: Arc<NvmeController>,
    cache: Arc<Mutex<HashMap<u64, Box<[u8; 4096]>>>>,
    pub len: usize,
}

impl Disk {
    pub async fn new() -> Result<Disk> {
        let ctrl = init_nvme().await.expect("failed to open nvme controller");
        let len = ctrl.flash_len().await;
        let len = std::cmp::max(len, u32::MAX as usize / SECTOR_SIZE);
        Ok(Disk {
            ctrl,
            cache: Arc::new(Mutex::new(HashMap::new())),
            len,
        })
    }

    pub fn lba_count(&self) -> usize {
        self.len / SECTOR_SIZE
    }
}

impl PagedDevice for Disk {
    async fn sequential_read(&self, start: u64, list: &[object_store::PhysRange]) -> Result<usize> {
        let phys = list
            .iter()
            .map(|r| {
                (r.start..r.end)
                    .into_iter()
                    .step_by(PAGE_SIZE)
                    .map(|addr| PhysInfo::new(PhysAddr(addr)))
            })
            .flatten()
            .collect::<Vec<_>>();
        let count = self
            .ctrl
            .sequential_read_async::<PAGE_SIZE>(start, phys.as_slice())
            .await?;
        Ok(count)
    }

    async fn sequential_write(
        &self,
        start: u64,
        list: &[object_store::PhysRange],
    ) -> Result<usize> {
        let phys = list
            .iter()
            .map(|r| {
                (r.start..r.end)
                    .into_iter()
                    .step_by(PAGE_SIZE)
                    .map(|addr| PhysInfo::new(PhysAddr(addr)))
            })
            .flatten()
            .collect::<Vec<_>>();
        let count = self
            .ctrl
            .sequential_write_async::<PAGE_SIZE>(start, phys.as_slice())
            .await?;
        Ok(count)
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.len)
    }

    async fn phys_addrs(
        &self,
        start: DevicePage,
        phys_list: &mut mayheap::Vec<PagedPhysMem, MAYHEAP_LEN>,
    ) -> Result<usize> {
        let ctx = PAGER_CTX.get().unwrap();
        let page = match ctx.data.try_alloc_page() {
            Ok(page) => page,
            Err(mw) => {
                tracing::debug!("OOM: (ok = {})", !phys_list.is_empty());
                if !phys_list.is_empty() {
                    return Ok(0);
                }
                block_on(mw)
            }
        };
        let phys_range = PhysRange::new(page, page + PAGE);
        let mut mem = PagedPhysMem::new(phys_range);
        if start.as_hole().is_some() {
            mem.set_completed();
        }
        phys_list.push(mem).unwrap();
        Ok(1)
    }

    fn yield_now(&self) {
        run_async(async {
            Timer::after(Duration::from_micros(100)).await;
        });
    }

    fn run_async<R: 'static>(&self, f: impl Future<Output = R>) -> R {
        run_async(f)
    }
}

impl PosIo for Disk {
    async fn read(&self, start: u64, buf: &mut [u8]) -> Result<usize> {
        let mut pos = start as usize;
        let mut lba = (pos / PAGE_SIZE) * 8;
        let mut bytes_written: usize = 0;
        let mut read_buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

        while bytes_written != buf.len() {
            if lba >= self.lba_count() {
                break;
            }

            let left = pos % PAGE_SIZE;
            let right = if left + buf.len() - bytes_written > PAGE_SIZE {
                PAGE_SIZE
            } else {
                left + buf.len() - bytes_written
            }; // If I want to write more than the boundary of a page

            self.ctrl
                .async_read_page(lba as u64, &mut read_buffer, 0)
                .await?;

            let bytes_to_read = right - left;
            buf[bytes_written..bytes_written + bytes_to_read]
                .copy_from_slice(&read_buffer[left..right]);

            bytes_written += bytes_to_read;
            pos += bytes_to_read;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_written)
    }

    async fn write(&self, start: u64, buf: &[u8]) -> Result<usize> {
        let mut pos = start as usize;
        let mut lba = (pos / PAGE_SIZE) * 8;
        let mut bytes_read = 0;
        let mut write_buffer: [u8; PAGE_SIZE] = [0; PAGE_SIZE];

        while bytes_read != buf.len() {
            if lba >= self.lba_count() {
                break;
            }

            let left = pos % PAGE_SIZE;
            let right = if left + buf.len() - bytes_read > PAGE_SIZE {
                PAGE_SIZE
            } else {
                left + buf.len() - bytes_read
            };
            if right - left != PAGE_SIZE {
                let temp_pos: u64 = pos.try_into().unwrap();
                // TODO: check if full read
                self.read(temp_pos & !(PAGE_SIZE - 1) as u64, &mut write_buffer)
                    .await?;
            }

            write_buffer[left..right].copy_from_slice(&buf[bytes_read..bytes_read + right - left]);
            bytes_read += right - left;

            pos += right - left;

            self.ctrl
                .async_write_page(lba as u64, &mut write_buffer, 0)
                .await?;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_read)
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
