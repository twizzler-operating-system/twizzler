use core::{
    fmt::Debug,
    sync::atomic::{AtomicUsize, Ordering},
};

use intrusive_collections::LinkedList;
use twizzler_abi::trace::{CONTEXT_INVALIDATION, CONTEXT_SHOOTDOWN, TraceEntryFlags, TraceKind};

use crate::{
    arch::{
        address::VirtAddr,
        context::ArchContextTarget,
        memory::pagetables::{ArchCacheLineMgr, ArchTlbMgr},
    },
    memory::frame::{FrameAdapter, FrameRef},
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

struct TlbStats {
    shootdowns: AtomicUsize,
    flushes: AtomicUsize,
}

static TLB_STATS: TlbStats = TlbStats {
    shootdowns: AtomicUsize::new(0),
    flushes: AtomicUsize::new(0),
};

pub fn fill_stats(stats: &mut twizzler_abi::syscall::MemoryStats) {
    stats.tlb_shootdown_count = TLB_STATS
        .shootdowns
        .load(core::sync::atomic::Ordering::SeqCst);
    stats.tlb_flush_count = TLB_STATS.flushes.load(core::sync::atomic::Ordering::SeqCst);
    // The switch counters live per-cpu (see `ProcessorStats`) so that incrementing them on the
    // switch path costs no cross-cpu traffic; summing them is this read side's job instead, and it
    // happens once per stat syscall.
    crate::processor::mp::with_each_active_processor(|p| {
        let s = &p.stats;
        stats.aspace_switch_flush_count += s.aspace_switch_flush.load(Ordering::Relaxed) as usize;
        stats.aspace_switch_noflush_count +=
            s.aspace_switch_noflush.load(Ordering::Relaxed) as usize;
        stats.tlb_revoke_count += s.aspace_flush_revoked.load(Ordering::Relaxed) as usize;
    });
}

/// Printed at shutdown beside the locktrack counters, and for the same reason: a run that finishes
/// cleanly has to put its numbers on the record, because silence is indistinguishable from a build
/// without the instrumentation.
///
/// `noflush / total` is the fraction of address-space switches that flushed before PCIDs and no
/// longer do -- self-contained, with no baseline run to compare against, because a flush was the
/// only outcome beforehand. `revoked` is what an invalidation-heavy workload takes back: it counts
/// only claims that actually existed to be revoked (see [ArchProcessor::pcid_invalidate]), so it is
/// a real count of forced future flushes rather than of revocation attempts, and if it approaches
/// `noflush` the feature is paying for itself and no more.
pub fn print_switch_counters() {
    let (mut noflush, mut flush, mut revoked) = (0u64, 0u64, 0u64);
    crate::processor::mp::with_each_active_processor(|p| {
        noflush += p.stats.aspace_switch_noflush.load(Ordering::Relaxed);
        flush += p.stats.aspace_switch_flush.load(Ordering::Relaxed);
        revoked += p.stats.aspace_flush_revoked.load(Ordering::Relaxed);
    });
    let total = noflush + flush;
    emerglogln!(
        "== aspace switches: {} total, {} noflush ({}%), {} flush, {} revoked",
        total,
        noflush,
        if total == 0 { 0 } else { noflush * 100 / total },
        flush,
        revoked
    );
}

pub fn tlb_shootdown_inc_count(ipi: bool) {
    TLB_STATS
        .flushes
        .fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    if ipi {
        TLB_STATS
            .shootdowns
            .fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    }
}

/// Management for consistency, wrapping any cache-line flushing, page-freeing, and TLB coherence
/// into a single object.
pub struct Consistency {
    cl: ArchCacheLineMgr,
    tlb: ArchTlbMgr,
    pages: LinkedList<FrameAdapter>,
}

impl Consistency {
    pub fn new(target: ArchContextTarget) -> Self {
        Self {
            cl: ArchCacheLineMgr::default(),
            tlb: ArchTlbMgr::new(target),
            pages: LinkedList::new(FrameAdapter::NEW),
        }
    }

    pub fn new_object_tables() -> Self {
        Self::new(ArchContextTarget::null())
    }

    #[cfg(target_arch = "x86_64")]
    pub fn new_full_global() -> Self {
        let mut this = Self::new(ArchContextTarget::null());
        this.set_full_global();
        this
    }

    /// Enqueue a TLB invalidation.
    pub fn enqueue(&mut self, addr: VirtAddr, is_global: bool, is_terminal: bool, level: usize) {
        self.tlb.enqueue(addr, is_global, is_terminal, level)
    }

    /// Flush a cache-line.
    pub fn add_cache_line(&mut self, addr: VirtAddr) {
        self.cl.add_cache_line(addr);
    }

    pub fn flush_cache(&mut self) {
        self.cl.flush();
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
