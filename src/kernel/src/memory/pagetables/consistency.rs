use intrusive_collections::LinkedList;

use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        memory::pagetables::{ArchCacheLineMgr, ArchTlbMgr},
    },
    memory::frame::{free_frame, FrameAdapter, FrameRef},
};

/// Management for consistency, wrapping any cache-line flushing and TLB coherence into a single object.
pub(super) struct Consistency {
    cl: ArchCacheLineMgr,
    tlb: ArchTlbMgr,
    pages: LinkedList<FrameAdapter>,
}

impl Consistency {
    pub(super) fn new(target: PhysAddr) -> Self {
        Self {
            cl: ArchCacheLineMgr::default(),
            tlb: ArchTlbMgr::new(target),
            pages: LinkedList::new(FrameAdapter::NEW),
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
    pub fn free_frame(&mut self, frame: FrameRef) {
        self.pages.push_back(frame);
    }

    /// Flush the TLB invalidations.
    fn flush_invalidations(&mut self) {
        self.tlb.finish();
    }

    pub(super) fn into_deferred(self) -> DeferredUnmappingOps {
        DeferredUnmappingOps { pages: self.pages }
    }
}

pub struct DeferredUnmappingOps {
    pages: LinkedList<FrameAdapter>,
}

impl Drop for DeferredUnmappingOps {
    fn drop(&mut self) {
        assert!(self.pages.is_empty());
    }
}

impl DeferredUnmappingOps {
    pub fn run_all(mut self) {
        while let Some(page) = self.pages.pop_back() {
            free_frame(page)
        }
    }
}
