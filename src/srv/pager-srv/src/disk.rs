use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    u32, u64,
};

use object_store::{DevicePage, PagedDevice, PagedPhysMem, PhysRange, PosIo, INLINE_LEN};
use twizzler::Result;
use twizzler_driver::dma::{PhysAddr, PhysInfo};

use crate::{
    helpers::PAGE,
    nvme::{init_nvme, NvmeController},
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
    pub fn new() -> Result<Disk> {
        let ctrl = init_nvme().expect("failed to open nvme controller");
        let len = ctrl.blocking_get_flash_size();
        // Warm the MDTS query here so the first I/O doesn't pay a blocking admin command.
        let _ = ctrl.blocking_get_max_transfer_pages::<PAGE_SIZE>();
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
    fn sequential_read(
        &self,
        start: u64,
        nr_pages: usize,
        list: &[object_store::PagedPhysMem],
        mut inner_cursor: usize,
    ) -> Result<usize> {
        // Describe one pipelined batch, not one command: `sequential_read_async` issues every
        // command in this range before awaiting any of them. Still bounded, because the caller
        // loops on the returned count -- describing every remaining page just to transfer a prefix
        // is what made a large run quadratic in its length.
        let nr_pages = nr_pages.min(self.ctrl.blocking_get_pipelined_pages::<PAGE_SIZE>());
        let mut i = 0;
        let mut cursor = 0;
        let mut phys = Vec::with_capacity(nr_pages);
        while i < nr_pages && cursor < list.len() {
            assert!(
                inner_cursor < list[cursor].nr_pages(),
                "inner_cursor {} is out of bounds for list[{}] with {} pages",
                inner_cursor,
                cursor,
                list[cursor].nr_pages()
            );
            let count = std::cmp::min(nr_pages - i, list[cursor].nr_pages() - inner_cursor);
            let start = list[cursor].range.start + (inner_cursor * PAGE_SIZE) as u64;

            // PRPTRACE (see nvme/dma.rs): catch garbage between the pool and the PRP builder.
            if start & 0xFFF as u64 != 0 {
                tracing::error!(
                    "PRPTRACE misaligned PagedPhysMem start {:x} (range {:x}..{:x}, inner {})",
                    start,
                    list[cursor].range.start,
                    list[cursor].range.end,
                    inner_cursor
                );
            }
            for j in 0..count {
                let phys_addr = start + (j * PAGE_SIZE) as u64;
                let phys_info = PhysInfo::new(PhysAddr(phys_addr));
                phys.push(phys_info);
            }

            i += count;
            inner_cursor = 0;
            cursor += 1;
        }

        let count = self
            .ctrl
            .sequential_read_async::<PAGE_SIZE>(start, phys.as_slice())?;
        Ok(count)
    }

    fn sequential_write(
        &self,
        start: u64,
        nr_pages: usize,
        list: &[object_store::PagedPhysMem],
        mut inner_cursor: usize,
    ) -> Result<usize> {
        // See `sequential_read`: clamp to one pipelined batch before building descriptors.
        let nr_pages = nr_pages.min(self.ctrl.blocking_get_pipelined_pages::<PAGE_SIZE>());
        let mut i = 0;
        let mut cursor = 0;
        let mut phys = Vec::with_capacity(nr_pages);
        while i < nr_pages && cursor < list.len() {
            assert!(inner_cursor < list[cursor].nr_pages());
            let count = std::cmp::min(nr_pages - i, list[cursor].nr_pages() - inner_cursor);
            let start = list[cursor].range.start + (inner_cursor * PAGE_SIZE) as u64;
            for j in 0..count {
                let phys_addr = start + (j * PAGE_SIZE) as u64;
                let phys_info = PhysInfo::new(PhysAddr(phys_addr));
                phys.push(phys_info);
            }
            i += count;
            inner_cursor = 0;
            cursor += 1;
        }

        let count = self
            .ctrl
            .sequential_write_async::<PAGE_SIZE>(start, phys.as_slice())?;
        Ok(count)
    }

    fn len(&self) -> Result<usize> {
        Ok(self.len)
    }

    fn phys_addrs(
        &self,
        start_obj_page: i64,
        nr_obj_pages: u32,
        pages: &[DevicePage],
        phys_list: &mut object_store::Vec<PagedPhysMem, INLINE_LEN>,
    ) -> Result<()> {
        assert!(phys_list.is_empty());
        let ctx = PAGER_CTX.get().unwrap();
        let mut count = 0;
        let mut inner_cursor = 0;
        let mut pages_cursor = 0;
        while count < nr_obj_pages && pages_cursor < pages.len() {
            let page = &pages[pages_cursor];
            let try_len = nr_obj_pages - count;
            let alloc = match ctx
                .data
                .try_alloc_pages(start_obj_page + count as i64, try_len)
            {
                Ok(x) => x,
                Err(mw) => {
                    tracing::debug!("OOM: (ok = {})", !phys_list.is_empty());
                    if !phys_list.is_empty() {
                        return Ok(());
                    }
                    tracing::info!(
                        "task out of memory, waiting (pool avail {} KB)",
                        ctx.data.avail_mem() / 1024
                    );
                    mw.wait();
                    continue;
                }
            };
            tracing::trace!(
                "phys_addrs: start_obj_page = {}, nr_obj_pages = {}, pages_cursor = {}, inner_cursor = {}, alloc = {:x} ({} pages)",
                start_obj_page,
                nr_obj_pages,
                pages_cursor,
                inner_cursor,
                alloc.0,
                alloc.1

            );

            let range = PhysRange::new(alloc.0, alloc.0 + PAGE * alloc.1 as u64);
            let mut mem = PagedPhysMem::new(range);
            if page.as_hole().is_some() {
                mem.set_completed();
            }
            phys_list.push(mem);

            if inner_cursor + alloc.1 as usize >= page.nr_pages() {
                inner_cursor = 0;
                pages_cursor += 1;
            } else {
                inner_cursor += alloc.1 as usize;
            }

            count += alloc.1;
        }
        Ok(())
    }

    fn yield_now(&self) {
        // An actual yield. This used to be `Timer::after(100us)` -- a real sleep, ~10ms per 10k
        // blocks mapped (pagerperf.md 6) -- and the timer was also the last thing on a worker path
        // that needed async-io's reactor, which `threads::park_poll` deliberately does not drive.
        std::thread::yield_now();
    }

    fn free_phys_range(&self, _range: PhysRange) {
        let ctx = PAGER_CTX.get().unwrap();
        ctx.data.add_memory_range(_range);
    }
}

impl PosIo for Disk {
    fn read(&self, start: u64, buf: &mut [u8]) -> Result<usize> {
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

            self.ctrl.async_read_page(lba as u64, &mut read_buffer, 0)?;

            let bytes_to_read = right - left;
            buf[bytes_written..bytes_written + bytes_to_read]
                .copy_from_slice(&read_buffer[left..right]);

            bytes_written += bytes_to_read;
            pos += bytes_to_read;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_written)
    }

    fn write(&self, start: u64, buf: &[u8]) -> Result<usize> {
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
                self.read(temp_pos & !(PAGE_SIZE - 1) as u64, &mut write_buffer)?;
            }

            write_buffer[left..right].copy_from_slice(&buf[bytes_read..bytes_read + right - left]);
            bytes_read += right - left;

            pos += right - left;

            self.ctrl
                .async_write_page(lba as u64, &mut write_buffer, 0)?;
            lba += PAGE_SIZE / SECTOR_SIZE;
        }

        Ok(bytes_read)
    }
}

pub mod benches {
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
        let ctrl = crate::disk::init_nvme().unwrap();
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
