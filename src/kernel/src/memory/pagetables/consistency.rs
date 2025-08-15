use intrusive_collections::LinkedList;
use twizzler_abi::trace::{TraceEntryFlags, TraceKind, CONTEXT_INVALIDATION, CONTEXT_SHOOTDOWN};

use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        memory::pagetables::{ArchCacheLineMgr, ArchTlbMgr},
    },
    memory::frame::{FrameAdapter, FrameRef},
    trace::{
        mgr::{TraceEvent, TRACE_MGR},
        new_trace_entry,
    },
};

/// Management for consistency, wrapping any cache-line flushing and TLB coherence into a single
/// object.
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
            crate::memory::tracker::free_frame(page)
        }
    }
}

pub fn trace_tlb_shootdown() {
    if TRACE_MGR.any_enabled(TraceKind::Context, CONTEXT_SHOOTDOWN) {
        let entry = new_trace_entry(
            TraceKind::Context,
            CONTEXT_SHOOTDOWN,
            TraceEntryFlags::empty(),
        );
        TRACE_MGR.async_enqueue(TraceEvent::new(entry));
    }
}

pub fn trace_tlb_invalidation() {
    if TRACE_MGR.any_enabled(TraceKind::Context, CONTEXT_INVALIDATION) {
        let entry = new_trace_entry(
            TraceKind::Context,
            CONTEXT_INVALIDATION,
            TraceEntryFlags::empty(),
        );
        TRACE_MGR.async_enqueue(TraceEvent::new(entry));
    }
}
