use crate::arch::{
    address::{PhysAddr, VirtAddr},
    memory::pagetables::{ArchCacheLineMgr, ArchTlbMgr},
};

pub struct Consistency {
    cl: ArchCacheLineMgr,
    tlb: ArchTlbMgr,
}

impl Consistency {
    pub fn new(target: PhysAddr) -> Self {
        Self {
            cl: ArchCacheLineMgr::default(),
            tlb: ArchTlbMgr::new(target),
        }
    }

    pub fn enqueue(&mut self, addr: VirtAddr, is_global: bool, is_terminal: bool, level: usize) {
        self.tlb.enqueue(addr, is_global, is_terminal, level)
    }

    pub fn flush(&self, addr: VirtAddr) {
        self.cl.flush(addr);
    }
}

impl Drop for Consistency {
    fn drop(&mut self) {
        self.tlb.finish();
    }
}
