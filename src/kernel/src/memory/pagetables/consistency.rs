use core::fmt::Debug;

use intrusive_collections::LinkedList;
use twizzler_abi::trace::{CONTEXT_INVALIDATION, CONTEXT_SHOOTDOWN, TraceEntryFlags, TraceKind};

use crate::{
    arch::{
        address::{PhysAddr, VirtAddr},
        memory::pagetables::{ArchCacheLineMgr, ArchTlbMgr},
    },
    memory::frame::{FrameAdapter, FrameRef},
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

/// Management for consistency, wrapping any cache-line flushing, page-freeing, and TLB coherence
/// into a single object.
pub struct Consistency {
    cl: ArchCacheLineMgr,
    tlb: ArchTlbMgr,
    pages: LinkedList<FrameAdapter>,
}

impl Consistency {
    pub fn new(target: PhysAddr) -> Self {
        Self {
            cl: ArchCacheLineMgr::default(),
            tlb: ArchTlbMgr::new(target),
            pages: LinkedList::new(FrameAdapter::NEW),
        }
    }

    pub fn new_object_tables() -> Self {
        Self::new(PhysAddr::new(0).unwrap())
    }

    #[cfg(target_arch = "x86_64")]
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
        if frame.dec_refcount() == 0 {
            self.pages.push_back(frame);
        }
    }

    /// Flush the TLB invalidations.
    fn flush_invalidations(&mut self) {
        self.tlb.finish();
    }

    pub fn into_deferred(self) -> DeferredUnmappingOps {
        assert!(!self.tlb.has_pending());
        DeferredUnmappingOps { pages: self.pages }
    }

    #[cfg(target_arch = "x86_64")]
    pub fn set_full_global(&mut self) {
        self.tlb.set_full_global();
    }

    pub fn tlb(&self) -> &ArchTlbMgr {
        &self.tlb
    }

    pub fn tlb_mut(&mut self) -> &mut ArchTlbMgr {
        &mut self.tlb
    }
}

pub struct DeferredUnmappingOps {
    pages: LinkedList<FrameAdapter>,
}

impl Debug for DeferredUnmappingOps {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeferredUnmappingOps").finish()
    }
}

impl Drop for DeferredUnmappingOps {
    fn drop(&mut self) {
        assert!(self.pages.is_empty());
    }
}

impl DeferredUnmappingOps {
    pub fn run_all(mut self) {
        while let Some(page) = self.pages.pop_back() {
            page.set_pt(false);
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
