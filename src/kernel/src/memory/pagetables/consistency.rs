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

/// Management for consistency, wrapping any cache-line flushing, page-freeing, and TLB coherence
/// into a single object.
pub struct Consistency {
    cl: ArchCacheLineMgr,
    tlb: ArchTlbMgr,
    pages: LinkedList<FrameAdapter>,
    shared: LinkedList<FrameAdapter>,
}

impl Consistency {
    pub fn new(target: PhysAddr) -> Self {
        Self {
            cl: ArchCacheLineMgr::default(),
            tlb: ArchTlbMgr::new(target),
            pages: LinkedList::new(FrameAdapter::NEW),
            shared: LinkedList::new(FrameAdapter::NEW),
        }
    }

    pub fn new_full_global() -> Self {
        let mut this = Self::new(unsafe { PhysAddr::new_unchecked(0) });
        this.set_full_global();
        this
    }

    /// Enqueue a TLB invalidation.
    pub fn enqueue(&mut self, addr: VirtAddr, is_global: bool, is_terminal: bool, level: usize) {
        self.tlb.enqueue(addr, is_global, is_terminal, level)
    }

    /// Flush a cache-line.
    pub fn flush(&mut self, addr: VirtAddr) {
        self.cl.flush(addr);
    }

    /// Enqueue a page for freeing.
    pub fn free_frame(&mut self, frame: FrameRef) {
        self.pages.push_back(frame);
    }

    /// Enqueue a page for freeing.
    pub fn free_shared_frame(&mut self, frame: FrameRef) {
        self.shared.push_back(frame);
    }

    /// Flush the TLB invalidations.
    fn flush_invalidations(&mut self) {
        self.tlb.finish();
    }

    pub fn into_deferred(self) -> DeferredUnmappingOps {
        DeferredUnmappingOps {
            pages: self.pages,
            shared: self.shared,
        }
    }

    pub fn set_full_global(&mut self) {
        self.tlb.set_full_global();
    }
}

pub struct DeferredUnmappingOps {
    pages: LinkedList<FrameAdapter>,
    shared: LinkedList<FrameAdapter>,
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

        while let Some(page) = self.shared.pop_back() {
            crate::memory::pagetables::free_shared_frame(page)
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
