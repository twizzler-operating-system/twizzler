use crate::arch::{
    address::{PhysAddr, VirtAddr},
    memory::pagetables::{ArchCacheLineMgr, ArchTlbMgr},
};

use super::PhysFrame;

/// Management for consistency, wrapping any cache-line flushing and TLB coherence into a single object.
pub(super) struct Consistency {
    cl: ArchCacheLineMgr,
    tlb: ArchTlbMgr,
}

impl Consistency {
    pub(super) fn new(target: PhysAddr) -> Self {
        Self {
            cl: ArchCacheLineMgr::default(),
            tlb: ArchTlbMgr::new(target),
        }
    }

    /// Enqueue a TLB invalidation.
    pub(super) fn enqueue(
        &mut self,
        addr: VirtAddr,
        is_global: bool,
        is_terminal: bool,
        level: usize,
    ) {
        self.tlb.enqueue(addr, is_global, is_terminal, level)
    }

    /// Flush a cache-line.
    pub(super) fn flush(&mut self, addr: VirtAddr) {
        self.cl.flush(addr);
    }

    /// Enqueue a page for freeing.
    pub fn free_page(&mut self, frame: PhysFrame) {
        todo!()
    }
}

impl Drop for Consistency {
    fn drop(&mut self) {
        self.tlb.finish();
        todo!("implement page freeing");
    }
}
