use std::sync::Mutex;

use twizzler_abi::pager::PhysRange;
use twizzler_driver::dma::DMA_PAGE_SIZE;

#[derive(Default)]
pub struct DramMgr {
    pool: Mutex<Vec<PhysRange>>,
}

pub struct PhysPage {
    addr: u64,
}

impl PhysPage {
    pub fn addr(&self) -> u64 {
        self.addr
    }
}

impl DramMgr {
    pub fn allocate_page(&self) -> Option<PhysPage> {
        let mut pool = self.pool.lock().unwrap();
        let last = pool.last_mut()?;
        if last.len >= DMA_PAGE_SIZE as u32 {
            last.len -= DMA_PAGE_SIZE as u32;
            let ret = last.start;
            last.start += DMA_PAGE_SIZE as u64;
            Some(PhysPage { addr: ret })
        } else {
            None
        }
    }

    pub fn add_range(&self, range: PhysRange) {
        let mut pool = self.pool.lock().unwrap();
        pool.push(range);
    }
}
