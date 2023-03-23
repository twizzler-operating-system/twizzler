use std::time::Duration;

use twizzler_driver::dma::DMA_PAGE_SIZE;

use crate::{
    datamgr::DataMgr,
    kernel::{KernelCommandQueue, PagerRequestQueue},
    memory::{DramMgr, PhysPage},
};

pub struct Pager {
    kq: KernelCommandQueue,
    pq: PagerRequestQueue,
    dram: DramMgr,
    data: DataMgr,
}

impl Pager {
    pub fn new(
        kq: KernelCommandQueue,
        pq: PagerRequestQueue,
        dram: DramMgr,
        data: DataMgr,
    ) -> Self {
        Self { kq, pq, dram, data }
    }

    pub async fn handler_main(&self) {
        loop {
            self.kq.handle_a_request(self).await;
        }
    }

    pub async fn allocate_page(&self) -> PhysPage {
        loop {
            let page = self.dram.allocate_page();
            if let Some(page) = page {
                return page;
            }
            let ranges = self.pq.request_dram(DMA_PAGE_SIZE * 32).await;
            if let Ok(ranges) = ranges {
                for range in &ranges {
                    if range.start != 0 && range.len != 0 {
                        self.dram.add_range(*range);
                    }
                }
            }
        }
    }

    pub async fn dram_manager_main(&self) {
        twizzler_async::Timer::after(Duration::from_secs(10)).await;
    }
}
