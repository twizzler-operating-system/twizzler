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
        memory::pagetables::{ArchCacheLineMgr, ArchTlbMgr, PendingShootdown},
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
    let (mut reasserted, mut dropped) = (0u64, 0u64);
    crate::processor::mp::with_each_active_processor(|p| {
        noflush += p.stats.aspace_switch_noflush.load(Ordering::Relaxed);
        flush += p.stats.aspace_switch_flush.load(Ordering::Relaxed);
        revoked += p.stats.aspace_flush_revoked.load(Ordering::Relaxed);
        reasserted += p.stats.aspace_claim_reasserted.load(Ordering::Relaxed);
        dropped += p.stats.aspace_claim_dropped.load(Ordering::Relaxed);
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
    // Printed apart from the line above rather than folded into it, because it is read against a
    // specific prediction: `revoked` should be unchanged (the sender still revokes everything)
    // while `flush` falls by roughly `reasserted`. A build where `reasserted` is large and `flush`
    // has not moved means the reclaimed claims are not the ones that were being spent, and the
    // mechanism is wrong however green the run is.
    emerglogln!(
        "== aspace claims: {} reasserted, {} dropped",
        reasserted,
        dropped
    );
}

/// Which page tables an invalidation came out of.
///
/// The two are worth telling apart because their costs are unrelated: an arch-mapper shootdown has
/// been through PCID revocation and normally targets zero or one cpu, while an object-table one is
/// merged across every context the object is mapped into and degrades to full+global as soon as
/// there are two of them -- which makes `should_target` true everywhere and costs a CR4.PGE toggle
/// per cpu. A single count cannot distinguish "most of the invalidations" from "most of the cost".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TlbOrigin {
    Arch = 0,
    Object = 1,
}

const NR_ORIGINS: usize = 2;
/// Target counts are bucketed as min(count, 7); the interesting resolution is all at the bottom,
/// since the question `MAX_TARGETED_IPIS` asks is how often the set is small.
const NR_BUCKETS: usize = 8;

struct ShootdownStats {
    /// Invalidations that reached the send phase, i.e. had something to invalidate.
    calls: [AtomicUsize; NR_ORIGINS],
    /// Of those, the ones that were full *and* global -- every cpu targeted, full flush each.
    global: [AtomicUsize; NR_ORIGINS],
    /// Sum of target counts, for a mean to read beside the histogram.
    targets: [AtomicUsize; NR_ORIGINS],
    hist: [[AtomicUsize; NR_BUCKETS]; NR_ORIGINS],
}

static SD_STATS: ShootdownStats = ShootdownStats {
    calls: [const { AtomicUsize::new(0) }; NR_ORIGINS],
    global: [const { AtomicUsize::new(0) }; NR_ORIGINS],
    targets: [const { AtomicUsize::new(0) }; NR_ORIGINS],
    hist: [const { [const { AtomicUsize::new(0) }; NR_BUCKETS] }; NR_ORIGINS],
};

pub fn tlb_shootdown_inc_count(count: usize, origin: TlbOrigin, global: bool) {
    TLB_STATS
        .flushes
        .fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    if count > 0 {
        TLB_STATS
            .shootdowns
            .fetch_add(1, core::sync::atomic::Ordering::SeqCst);
    }
    let o = origin as usize;
    SD_STATS.calls[o].fetch_add(1, Ordering::Relaxed);
    SD_STATS.targets[o].fetch_add(count, Ordering::Relaxed);
    SD_STATS.hist[o][count.min(NR_BUCKETS - 1)].fetch_add(1, Ordering::Relaxed);
    if global {
        SD_STATS.global[o].fetch_add(1, Ordering::Relaxed);
    }
}

/// Time actually spent spinning for remote acknowledgement, per origin.
///
/// This is the number that decides whether moving the wait out from under the object page-table
/// mutex is worth the churn: an object-origin wait happens with that mutex held, so this is exactly
/// the hold time that would be recovered. Measured rather than assumed, because after the
/// per-context precision fix most sends have no targets at all and cost nothing to "wait" for.
static WAIT_NS: [AtomicUsize; NR_ORIGINS] = [const { AtomicUsize::new(0) }; NR_ORIGINS];
static WAIT_N: [AtomicUsize; NR_ORIGINS] = [const { AtomicUsize::new(0) }; NR_ORIGINS];
/// Longest single wait seen, which a total plus a mean cannot show -- a rare multi-millisecond
/// stall and a uniform smear have very different consequences for a lock hold.
static WAIT_MAX_NS: [AtomicUsize; NR_ORIGINS] = [const { AtomicUsize::new(0) }; NR_ORIGINS];

pub fn tlb_wait_record(origin: TlbOrigin, elapsed: core::time::Duration) {
    let o = origin as usize;
    let ns = elapsed.as_nanos() as usize;
    WAIT_NS[o].fetch_add(ns, Ordering::Relaxed);
    WAIT_N[o].fetch_add(1, Ordering::Relaxed);
    WAIT_MAX_NS[o].fetch_max(ns, Ordering::Relaxed);
}

/// Printed beside [print_switch_counters], and for the same reason.
pub fn print_shootdown_counters() {
    // Expected large: one lock hold runs many operations. Each is a merge, not a discharge, so no
    // wait happens inside the lock however big this gets.
    emerglogln!(
        "== tlb merged parks: {}",
        crate::obj::pagetables::merged_parks::count()
    );
    for (o, name) in [(0usize, "arch"), (1, "obj")] {
        let n = WAIT_N[o].load(Ordering::Relaxed);
        let total = WAIT_NS[o].load(Ordering::Relaxed);
        emerglogln!(
            "== tlb waits ({}): {} waits, {} us total, {} ns mean, {} ns max",
            name,
            n,
            total / 1000,
            if n == 0 { 0 } else { total / n },
            WAIT_MAX_NS[o].load(Ordering::Relaxed)
        );
    }
    for (o, name) in [(0usize, "arch"), (1, "obj")] {
        let calls = SD_STATS.calls[o].load(Ordering::Relaxed);
        if calls == 0 {
            emerglogln!("== tlb shootdowns ({}): none", name);
            continue;
        }
        let targets = SD_STATS.targets[o].load(Ordering::Relaxed);
        let global = SD_STATS.global[o].load(Ordering::Relaxed);
        let mut hist = [0usize; NR_BUCKETS];
        for (i, h) in hist.iter_mut().enumerate() {
            *h = SD_STATS.hist[o][i].load(Ordering::Relaxed);
        }
        emerglogln!(
            "== tlb shootdowns ({}): {} sends, {} global ({}%), {} targets total ({}/100 mean), hist[0..6,7+] {} {} {} {} {} {} {} {}",
            name,
            calls,
            global,
            global * 100 / calls,
            targets,
            targets * 100 / calls,
            hist[0],
            hist[1],
            hist[2],
            hist[3],
            hist[4],
            hist[5],
            hist[6],
            hist[7]
        );
    }
}

/// Management for consistency, wrapping any cache-line flushing, page-freeing, and TLB coherence
/// into a single object.
pub struct Consistency {
    cl: ArchCacheLineMgr,
    tlb: ArchTlbMgr,
    pages: LinkedList<FrameAdapter>,
    /// Set by [Self::finish_send], handed to the [DeferredUnmappingOps] so that the frames cannot
    /// be freed before the shootdown is acknowledged.
    pending: Option<PendingShootdown>,
    /// Leaf-entry pages added (+) or removed (-) by this operation, in 4 KiB units.
    ///
    /// Accumulated by [`super::Table::update_entry`] -- the single place every entry write goes
    /// through -- and drained by [`super::Mapper`] at the same points it bumps its generation, so
    /// `Mapper::page_count` is exact without walking. `Consistency` is the only thing already
    /// threaded through the whole recursion, which is why it carries this.
    page_delta: isize,
}

impl Consistency {
    pub fn new(target: ArchContextTarget) -> Self {
        Self {
            cl: ArchCacheLineMgr::default(),
            tlb: ArchTlbMgr::new(target),
            pages: LinkedList::new(FrameAdapter::NEW),
            pending: None,
            page_delta: 0,
        }
    }

    /// Record a leaf entry appearing (+) or disappearing (-), in 4 KiB units.
    pub fn add_page_delta(&mut self, delta: isize) {
        self.page_delta += delta;
    }

    /// Take the accumulated delta, resetting it. Called once per [`super::Mapper`] operation.
    pub fn take_page_delta(&mut self) -> isize {
        core::mem::replace(&mut self.page_delta, 0)
    }

    pub fn new_object_tables() -> Self {
        let mut this = Self::new(ArchContextTarget::null());
        this.tlb.set_origin(TlbOrigin::Object);
        this
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

    /// Send the queued invalidations without waiting for them to be acknowledged, parking the
    /// obligation to wait on this object. It travels from here into the [DeferredUnmappingOps],
    /// which discharges it before freeing anything -- so a caller can drop its page-table lock in
    /// between, and the wait moves out of the lock hold without the ordering being weakened.
    pub fn finish_send(&mut self) {
        let pending = self.tlb.finish_send();
        self.set_pending(pending);
    }

    /// Park an obligation produced elsewhere -- the object page tables build their own
    /// [ArchTlbMgr]s per context rather than sending this object's.
    pub fn set_pending(&mut self, pending: PendingShootdown) {
        assert!(self.pending.is_none(), "shootdown already in flight");
        self.pending = Some(pending);
    }

    /// Note that `!has_pending()` no longer means "the shootdown is complete" -- the send half
    /// resets the invalidation data. What guarantees completion is the token moved out below.
    pub fn into_deferred(self) -> DeferredUnmappingOps {
        assert!(!self.tlb.has_pending());
        DeferredUnmappingOps {
            pages: self.pages,
            pending: self.pending,
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub fn set_full_global(&mut self) {
        self.tlb.set_full_global();
    }

    /// See [ArchTlbMgr::set_full].
    pub fn set_full(&mut self) {
        self.tlb.set_full();
    }

    /// Nothing to invalidate, nothing to free, nothing already parked.
    ///
    /// The *enqueue* is already skipped for a not-present -> present transition --
    /// `Table::update_entry` only calls `consist.enqueue` `if was_present`, and a zero-fill fault
    /// is by definition not present. What is not skipped is everything built around the
    /// invalidation that was never enqueued: `PendingShootdown::none()` (a 1,024-cpu `CpuSet`,
    /// 128 bytes, zeroed), `ArchTlbMgr::reset` (a 16-entry instruction array rewritten), three or
    /// four moves of those through `set_pending` / `into_deferred` / `park`, and a
    /// `DeferredUnmappingOps` parked on the object for a later `run_all` with nothing in it.
    /// Measured at **396 ns per page** on `page_fault_zero_fill` (`knobs-on`), of which 214 ns is
    /// the park half alone -- against zero TLB work performed.
    pub fn is_trivial(&self) -> bool {
        !self.tlb.has_pending() && self.pages.is_empty() && self.pending.is_none()
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
    pending: Option<PendingShootdown>,
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
    /// Work consisting only of a shootdown to wait for, with no frames to free -- what
    /// [ObjectPageTable::invalidate] produces, so that it can be parked the same way.
    pub fn from_pending(pending: PendingShootdown) -> Self {
        Self {
            pages: LinkedList::new(FrameAdapter::NEW),
            pending: Some(pending),
        }
    }

    /// Take on another batch's work, so that several operations under one lock hold discharge once
    /// at the end rather than each displacing the last.
    ///
    /// Both halves are O(1) and allocation-free: [PendingShootdown::absorb] unions the target sets,
    /// and `pages` is an intrusive list, so joining two is a pointer splice. That is what makes
    /// merging strictly better than the discharge-the-old-one-inline alternative -- it removes the
    /// in-lock wait *by construction*, rather than by relying on lock holds being short. They are
    /// not: a hold runs one consistency-generating operation per page of a page-in or copy loop.
    pub fn absorb(&mut self, mut other: Self) {
        self.pages.back_mut().splice_after(other.pages.take());
        match (self.pending.as_mut(), other.pending.take()) {
            (Some(ours), Some(theirs)) => ours.absorb(theirs),
            (None, theirs) => self.pending = theirs,
            (Some(_), None) => {}
        }
    }

    pub fn run_all(mut self) {
        // Wait first, free second, and in that order for two reasons: no frame may go back to the
        // allocator while a processor can still reach it through a stale entry, and the wait is
        // also what ends the shootdown -- freeing reaches the tracker, which can wake the reclaim
        // thread, and that belongs outside rather than inside.
        if let Some(pending) = self.pending.take() {
            pending.wait();
        }
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
