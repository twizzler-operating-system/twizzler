//! Counters for the pager path, dumped at `debug_shutdown`.
//!
//! Unlike [`crate::syscall::SYSCALL_PROFILE`] and its siblings there is no switch, because there is
//! nothing to switch off. The pager is entered on the order of seventy times in a boot, so the
//! per-request counters cannot distort what they measure; the per-page ones are relaxed adds on the
//! path that is already moving a page of data. Nothing prints until shutdown.
//!
//! Two questions shaped what is here, both from `sysperf.md` round 6:
//!
//! - **How many pages does a fault ask for against how many it needs?** `pages_requested` against
//!   the fault count says it directly. The userspace side already reports the pages it *served*
//!   (`LANESTATS`); this is the kernel's own account of what it asked for, which is the half that
//!   the read-ahead widening controls.
//! - **Is the pager's concurrency demand-limited or capacity-limited?** `pager-srv`'s `REQSTATS`
//!   reports "max 2 demand in flight" and cannot distinguish "never asked for more" from "could not
//!   take more". `depth` is the kernel's side of that: how many requests were already outstanding
//!   each time it issued another. If it is all in the first bucket, there was never a second
//!   request to overlap and concurrency is not the lead.

use core::sync::atomic::{AtomicU64, Ordering};

/// Upper bounds for the outstanding-at-submit histogram; the last bucket is everything above.
const DEPTH_BOUNDS: [u64; 5] = [1, 2, 4, 8, 16];
const NR_DEPTH: usize = DEPTH_BOUNDS.len() + 1;

/// Upper bounds for request latency, in microseconds; the last bucket is everything above.
const LAT_US: [u64; 3] = [100, 1_000, 10_000];
const NR_LAT: usize = LAT_US.len() + 1;

pub struct PagerProfile {
    /// Requests that got their own slot and were sent.
    submitted: AtomicU64,
    /// Of those, the ones asking for page data. The rest are info/create/sync/delete, which name
    /// no pages, so this is the denominator for `pages_requested`.
    page_reqs: AtomicU64,
    /// Requests that found an identical (or prefetch-twin) request already in flight.
    coalesced: AtomicU64,
    /// Requests refused because every slot was taken.
    no_slot: AtomicU64,
    /// Requests that waited on an in-flight request already covering their range instead of
    /// sending a second one. These are the duplicate transfers that used to happen.
    covered: AtomicU64,
    /// Requests that waited on one covering only their caller's blocking pages, sending nothing
    /// and giving up their read-ahead. The trade this makes is visible as `installed` against
    /// the previous run, not here.
    covered_required: AtomicU64,
    /// Requests whose range started with pages the object had acquired since the range was built,
    /// and how many such pages were trimmed off before sending. These used to be transferred and
    /// thrown away on arrival; nothing in flight explains them, because the request that installed
    /// them had already completed. See the window described in `get_pages_and_wait`.
    asked_present_reqs: AtomicU64,
    asked_present_pages: AtomicU64,
    asked_present_max: AtomicU64,
    /// Speculative requests -- ones nobody is blocked on -- reaching the submit path, and how many
    /// of them left without sending anything because every page was already there. Read-ahead is
    /// the thing all of this risks suppressing, so it is counted apart from demand rather than
    /// inferred from `installed` moving.
    spec_reqs: AtomicU64,
    spec_skipped: AtomicU64,
    spec_pages: AtomicU64,
    /// Wait to acquire the global inflight-manager mutex, and the per-send `idmap` spinlock.
    ///
    /// Both are taken on every submit *and* every completion, and the manager lock additionally on
    /// every turn of the wait loop -- so they are the two places the request-management path can
    /// serialize independently of the pager. Uninteresting while nothing overlapped; now that
    /// splitting per region puts up to ten requests in flight, worth knowing before adding more.
    mgr_lock_ns: AtomicU64,
    mgr_lock_max_ns: AtomicU64,
    mgr_lock_count: AtomicU64,
    mgr_lock_slow: AtomicU64,
    idmap_lock_ns: AtomicU64,
    idmap_lock_max_ns: AtomicU64,
    idmap_lock_count: AtomicU64,
    idmap_lock_slow: AtomicU64,
    /// Requests already marked done by the time their slot was released, against those the DONE
    /// flag alone completed, plus the pages they asked for that never came. `remaining_pages` is
    /// seeded from the full ask and the pager clamps before transferring, so a request naming
    /// pages past EOF can only ever finish by flag -- this says how many do, and how big the
    /// shortfall is.
    fulfilled_by_count: AtomicU64,
    fulfilled_by_flag: AtomicU64,
    unfulfilled_pages: AtomicU64,
    /// The EOF clamp on the read-ahead widening: how often it applied and what it saved, against
    /// the two reasons it declines. `no_len` is the one that matters -- the clamp can only act on
    /// an object whose length the pager has stated, and if that is most of them the ask stays
    /// inflated no matter how well the trim itself works.
    eof_clamped: AtomicU64,
    eof_clamped_pages: AtomicU64,
    eof_no_len: AtomicU64,
    eof_past_end: AtomicU64,
    /// Requests narrowed against ranges already in flight, and the pages that narrowing dropped.
    /// Speculative pages only -- a page the caller is blocked on is never given up.
    narrowed: AtomicU64,
    narrowed_pages: AtomicU64,
    /// Demand faults past EOF served with zeroed frames instead of a pager round trip, and the
    /// pages that saved. `declined_meta` / `declined_absent` are the two reasons the floor check
    /// bailed: the caller's range reached into the metadata region, or the meta page was not
    /// resident so the floor was unknown.
    zero_fill_reqs: AtomicU64,
    zero_fill_pages: AtomicU64,
    zero_fill_declined_meta: AtomicU64,
    zero_fill_declined_absent: AtomicU64,
    /// Object pages named by submitted requests. Against the pager-fault count, this is the read
    /// amplification the widening in `ensure_in_core_pager` produces.
    pages_requested: AtomicU64,
    /// Requests already outstanding when another was submitted.
    depth: [AtomicU64; NR_DEPTH],
    max_depth: AtomicU64,

    lat_ns_sum: AtomicU64,
    lat_ns_max: AtomicU64,
    lat_count: AtomicU64,
    lat_bucket: [AtomicU64; NR_LAT],

    /// Creation to queue submit: the kernel's own submit path, and the only part of a round trip
    /// that is over before the pager can see the request.
    pre_ns_sum: AtomicU64,
    pre_ns_max: AtomicU64,
    /// Submit to the first completion handled. Nothing in this segment is the kernel's: it is
    /// queue transit, the pager's scheduling, its store, and its write-back, and it is the
    /// segment the measurement in `INPROG.md` left unexplained.
    wait_ns_sum: AtomicU64,
    wait_ns_max: AtomicU64,
    /// First completion to slot release -- how spread out a multi-completion answer is.
    spread_ns_sum: AtomicU64,
    spread_ns_max: AtomicU64,
    /// Requests that reached release having been both submitted and answered, so the three
    /// segments above have a denominator that is not `lat_count` (which counts coalesced ones
    /// too).
    split_count: AtomicU64,

    /// Time spent inside `pager_compl_handle_page_data` installing pages. A component of the
    /// spread above rather than a fourth disjoint segment, and the part item 4 set out to
    /// shrink.
    install_ns_sum: AtomicU64,
    install_ns_max: AtomicU64,

    /// Page-data completions handled.
    completions: AtomicU64,
    /// Completions carrying at least one page the object already had.
    completions_with_dup: AtomicU64,
    /// Pages arriving in page-data completions.
    delivered: AtomicU64,
    /// Pages that went into the object's page tables.
    installed: AtomicU64,
    /// Pages dropped because the object had already acquired that page. Transferred, mapped by the
    /// pager, handed over, and freed again -- the whole cost paid for nothing.
    duplicate: AtomicU64,
    /// Of those, the ones rejected because a large page already covered the offset rather than a
    /// 4 KiB page. A request that loses the race to a merge has its whole 2 MiB region rejected,
    /// so this separates "a few overlaps amplified by merges" from "many small overlaps".
    duplicate_large: AtomicU64,
    /// Completions where 512 pages merged into one large frame.
    merged_large: AtomicU64,
}

pub static PAGER_PROFILE: PagerProfile = PagerProfile {
    submitted: AtomicU64::new(0),
    page_reqs: AtomicU64::new(0),
    coalesced: AtomicU64::new(0),
    no_slot: AtomicU64::new(0),
    covered: AtomicU64::new(0),
    covered_required: AtomicU64::new(0),
    asked_present_reqs: AtomicU64::new(0),
    asked_present_pages: AtomicU64::new(0),
    asked_present_max: AtomicU64::new(0),
    spec_reqs: AtomicU64::new(0),
    spec_skipped: AtomicU64::new(0),
    spec_pages: AtomicU64::new(0),
    mgr_lock_ns: AtomicU64::new(0),
    mgr_lock_max_ns: AtomicU64::new(0),
    mgr_lock_count: AtomicU64::new(0),
    mgr_lock_slow: AtomicU64::new(0),
    idmap_lock_ns: AtomicU64::new(0),
    idmap_lock_max_ns: AtomicU64::new(0),
    idmap_lock_count: AtomicU64::new(0),
    idmap_lock_slow: AtomicU64::new(0),
    fulfilled_by_count: AtomicU64::new(0),
    fulfilled_by_flag: AtomicU64::new(0),
    unfulfilled_pages: AtomicU64::new(0),
    eof_clamped: AtomicU64::new(0),
    eof_clamped_pages: AtomicU64::new(0),
    eof_no_len: AtomicU64::new(0),
    eof_past_end: AtomicU64::new(0),
    narrowed: AtomicU64::new(0),
    narrowed_pages: AtomicU64::new(0),
    zero_fill_reqs: AtomicU64::new(0),
    zero_fill_pages: AtomicU64::new(0),
    zero_fill_declined_meta: AtomicU64::new(0),
    zero_fill_declined_absent: AtomicU64::new(0),
    pages_requested: AtomicU64::new(0),
    depth: [const { AtomicU64::new(0) }; NR_DEPTH],
    max_depth: AtomicU64::new(0),
    lat_ns_sum: AtomicU64::new(0),
    lat_ns_max: AtomicU64::new(0),
    lat_count: AtomicU64::new(0),
    lat_bucket: [const { AtomicU64::new(0) }; NR_LAT],
    pre_ns_sum: AtomicU64::new(0),
    pre_ns_max: AtomicU64::new(0),
    wait_ns_sum: AtomicU64::new(0),
    wait_ns_max: AtomicU64::new(0),
    spread_ns_sum: AtomicU64::new(0),
    spread_ns_max: AtomicU64::new(0),
    split_count: AtomicU64::new(0),
    install_ns_sum: AtomicU64::new(0),
    install_ns_max: AtomicU64::new(0),
    completions: AtomicU64::new(0),
    completions_with_dup: AtomicU64::new(0),
    delivered: AtomicU64::new(0),
    installed: AtomicU64::new(0),
    duplicate: AtomicU64::new(0),
    duplicate_large: AtomicU64::new(0),
    merged_large: AtomicU64::new(0),
};

/// Where `lookup_object_and_wait` spends a call, split into the three things it can do: consult the
/// in-kernel object map, submit an info request, and sleep until the completion handler has
/// registered the object.
///
/// It exists because the map syscall's cost was traced to this function by subtraction
/// (`INPROG.md`, next step 1) and subtraction cannot say *which part* -- a slow in-kernel lookup
/// (two sleeping-mutex acquisitions on a global map) and a slow pager round trip are both
/// consistent with the same total.
pub mod lookupstats {
    use core::sync::atomic::{AtomicU64, Ordering};

    static CALLS: AtomicU64 = AtomicU64::new(0);
    /// Calls that never asked the pager -- the object was already registered.
    static HITS: AtomicU64 = AtomicU64::new(0);
    /// Trips round the loop, over all calls. Above `CALLS - HITS` means a wake found the object
    /// still absent.
    static ITERS: AtomicU64 = AtomicU64::new(0);
    static MAP_NS: AtomicU64 = AtomicU64::new(0);
    static SUBMIT_NS: AtomicU64 = AtomicU64::new(0);
    static BLOCK_NS: AtomicU64 = AtomicU64::new(0);
    static BLOCK_MAX: AtomicU64 = AtomicU64::new(0);
    static TOTAL_NS: AtomicU64 = AtomicU64::new(0);

    /// One trip round the loop: `map` is the in-kernel lookup that opened it.
    pub fn iteration(map_ns: u64) {
        ITERS.fetch_add(1, Ordering::Relaxed);
        MAP_NS.fetch_add(map_ns, Ordering::Relaxed);
    }

    pub fn submitted(ns: u64) {
        SUBMIT_NS.fetch_add(ns, Ordering::Relaxed);
    }

    pub fn blocked(ns: u64) {
        BLOCK_NS.fetch_add(ns, Ordering::Relaxed);
        BLOCK_MAX.fetch_max(ns, Ordering::Relaxed);
    }

    /// The last moment an object-info completion finished being handled and its waiter was marked
    /// ready, and how long that handling took.
    ///
    /// A single slot rather than a per-request stamp: the point is to cut `blocked` at the
    /// completion, and info requests are rare (53 in a boot) and almost never concurrent, so the
    /// one case this cannot serve -- two in flight at once -- is detected and dropped by
    /// [`woke`] rather than silently averaged in.
    static LAST_READY_NS: AtomicU64 = AtomicU64::new(0);
    static HANDLE_NS: AtomicU64 = AtomicU64::new(0);
    static HANDLE_COUNT: AtomicU64 = AtomicU64::new(0);
    /// Split of `blocked` at the completion: everything up to the waiter being made runnable, and
    /// the wait for a CPU after it.
    static TO_READY_NS: AtomicU64 = AtomicU64::new(0);
    static READY_TO_WAKE_NS: AtomicU64 = AtomicU64::new(0);
    static WAKE_MAX: AtomicU64 = AtomicU64::new(0);
    static SPLIT_COUNT: AtomicU64 = AtomicU64::new(0);
    /// Wakes whose ready stamp did not belong to them -- two info requests overlapping, or a stamp
    /// from before this call was submitted. Reported so the split above is read against a
    /// denominator that is not assumed.
    static SPLIT_AMBIG: AtomicU64 = AtomicU64::new(0);

    pub fn info_ready(ready_ns: u64, handle_ns: u64) {
        HANDLE_NS.fetch_add(handle_ns, Ordering::Relaxed);
        HANDLE_COUNT.fetch_add(1, Ordering::Relaxed);
        LAST_READY_NS.store(ready_ns, Ordering::Release);
    }

    /// A waiter woke at `now_ns`, having submitted at `submitted_ns`.
    pub fn woke(submitted_ns: u64, now_ns: u64) {
        let ready = LAST_READY_NS.load(Ordering::Acquire);
        if ready <= submitted_ns || ready > now_ns {
            SPLIT_AMBIG.fetch_add(1, Ordering::Relaxed);
            return;
        }
        TO_READY_NS.fetch_add(ready - submitted_ns, Ordering::Relaxed);
        READY_TO_WAKE_NS.fetch_add(now_ns - ready, Ordering::Relaxed);
        WAKE_MAX.fetch_max(now_ns - ready, Ordering::Relaxed);
        SPLIT_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    pub fn finished(total_ns: u64, hit: bool) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        TOTAL_NS.fetch_add(total_ns, Ordering::Relaxed);
        if hit {
            HITS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn print() {
        let calls = CALLS.load(Ordering::Relaxed);
        if calls == 0 {
            return;
        }
        let hits = HITS.load(Ordering::Relaxed);
        let iters = ITERS.load(Ordering::Relaxed).max(1);
        // Hits return without submitting anything, so the pager segments are only meaningful
        // against the calls that missed. `total` covers both and is reported over `calls`.
        let missed = calls.saturating_sub(hits).max(1);
        logln!(
            "  lookup_object_and_wait: {} calls ({} answered from the object map), {} loop trips, \
             mean {} us per call",
            calls,
            hits,
            iters,
            TOTAL_NS.load(Ordering::Relaxed) / calls / 1000,
        );
        logln!(
            "    object map {} ns per trip; per call that asked the pager: submit {} ns, blocked \
             {} us (max {} us)",
            MAP_NS.load(Ordering::Relaxed) / iters,
            SUBMIT_NS.load(Ordering::Relaxed) / missed,
            BLOCK_NS.load(Ordering::Relaxed) / missed / 1000,
            BLOCK_MAX.load(Ordering::Relaxed) / 1000,
        );
        let split = SPLIT_COUNT.load(Ordering::Relaxed);
        if split > 0 {
            logln!(
                "    of that block, over {} unambiguous ({} dropped): {} us to the completion \
                 being handled, then {} us waiting for a cpu (max {} us); handling itself {} ns",
                split,
                SPLIT_AMBIG.load(Ordering::Relaxed),
                TO_READY_NS.load(Ordering::Relaxed) / split / 1000,
                READY_TO_WAKE_NS.load(Ordering::Relaxed) / split / 1000,
                WAKE_MAX.load(Ordering::Relaxed) / 1000,
                HANDLE_NS.load(Ordering::Relaxed) / HANDLE_COUNT.load(Ordering::Relaxed).max(1),
            );
        }
    }
}

fn bucket(value: u64, bounds: &[u64]) -> usize {
    bounds
        .iter()
        .position(|b| value <= *b)
        .unwrap_or(bounds.len())
}

impl PagerProfile {
    /// A new request took a slot. `outstanding` counts the ones already in flight, not including
    /// this one.
    pub fn submitted(&self, outstanding: usize, pages: usize) {
        self.submitted.fetch_add(1, Ordering::Relaxed);
        if pages > 0 {
            self.page_reqs.fetch_add(1, Ordering::Relaxed);
            self.pages_requested
                .fetch_add(pages as u64, Ordering::Relaxed);
        }
        self.depth[bucket(outstanding as u64, &DEPTH_BOUNDS)].fetch_add(1, Ordering::Relaxed);
        self.max_depth
            .fetch_max(outstanding as u64 + 1, Ordering::Relaxed);
    }

    pub fn coalesced(&self) {
        self.coalesced.fetch_add(1, Ordering::Relaxed);
    }

    pub fn no_slot(&self) {
        self.no_slot.fetch_add(1, Ordering::Relaxed);
    }

    /// A request waited on one already covering its range rather than sending.
    pub fn covered(&self) {
        self.covered.fetch_add(1, Ordering::Relaxed);
    }

    /// A request waited on one covering only the pages its caller blocks on, giving up the
    /// read-ahead it would have asked for.
    pub fn covered_required(&self) {
        self.covered_required.fetch_add(1, Ordering::Relaxed);
    }

    /// A request is about to go out whose range starts with `pages` the object already has.
    pub fn asked_for_present(&self, pages: usize) {
        self.asked_present_reqs.fetch_add(1, Ordering::Relaxed);
        self.asked_present_pages
            .fetch_add(pages as u64, Ordering::Relaxed);
        self.asked_present_max
            .fetch_max(pages as u64, Ordering::Relaxed);
    }

    /// A speculative request reached the submit path asking for `pages`, and either sent or --
    /// if `skipped` -- found everything already present and sent nothing.
    pub fn speculative(&self, pages: usize, skipped: bool) {
        self.spec_reqs.fetch_add(1, Ordering::Relaxed);
        self.spec_pages.fetch_add(pages as u64, Ordering::Relaxed);
        if skipped {
            self.spec_skipped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Anything past this counts as contended. Well above an uncontended acquisition (a handful of
    /// cycles) and well below a scheduler round trip, so it separates "waited" from "walked in".
    const SLOW_LOCK_NS: u64 = 1_000;

    /// Time spent acquiring the inflight-manager mutex.
    pub fn mgr_lock(&self, ns: u64) {
        self.mgr_lock_ns.fetch_add(ns, Ordering::Relaxed);
        self.mgr_lock_max_ns.fetch_max(ns, Ordering::Relaxed);
        self.mgr_lock_count.fetch_add(1, Ordering::Relaxed);
        if ns > Self::SLOW_LOCK_NS {
            self.mgr_lock_slow.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Time spent acquiring the sender's `idmap` spinlock.
    pub fn idmap_lock(&self, ns: u64) {
        self.idmap_lock_ns.fetch_add(ns, Ordering::Relaxed);
        self.idmap_lock_max_ns.fetch_max(ns, Ordering::Relaxed);
        self.idmap_lock_count.fetch_add(1, Ordering::Relaxed);
        if ns > Self::SLOW_LOCK_NS {
            self.idmap_lock_slow.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A request's slot was released. `was_done` says the page count (or an info/error path) had
    /// already completed it; `remaining` is what it asked for and never received.
    pub fn fulfillment(&self, was_done: bool, remaining: usize) {
        if was_done {
            self.fulfilled_by_count.fetch_add(1, Ordering::Relaxed);
        } else {
            self.fulfilled_by_flag.fetch_add(1, Ordering::Relaxed);
        }
        self.unfulfilled_pages
            .fetch_add(remaining as u64, Ordering::Relaxed);
    }

    /// The read-ahead widening was trimmed to the object's data length, dropping `pages`.
    pub fn eof_clamped(&self, pages: usize) {
        self.eof_clamped.fetch_add(1, Ordering::Relaxed);
        self.eof_clamped_pages
            .fetch_add(pages as u64, Ordering::Relaxed);
    }

    /// The clamp declined: the pager never stated a length for this object.
    pub fn eof_no_len(&self) {
        self.eof_no_len.fetch_add(1, Ordering::Relaxed);
    }

    /// The clamp declined: the caller's own range already reaches past the data length, which is
    /// the object-metadata region rather than read-ahead.
    pub fn eof_past_end(&self) {
        self.eof_past_end.fetch_add(1, Ordering::Relaxed);
    }

    /// A request was narrowed against ranges in flight, dropping `pages` speculative pages.
    pub fn narrowed(&self, pages: usize) {
        self.narrowed.fetch_add(1, Ordering::Relaxed);
        self.narrowed_pages
            .fetch_add(pages as u64, Ordering::Relaxed);
    }

    /// A demand fault past EOF was backed with `pages` zeroed frames rather than a pager trip.
    pub fn zero_filled(&self, pages: usize) {
        self.zero_fill_reqs.fetch_add(1, Ordering::Relaxed);
        self.zero_fill_pages
            .fetch_add(pages as u64, Ordering::Relaxed);
    }

    /// A past-EOF fault could not be zero-filled. `meta_resident` distinguishes "range reached the
    /// metadata region" (true) from "meta page not resident, floor unknown" (false) -- the latter
    /// is the one that means the saving is being left on the table for want of a resident meta
    /// page.
    pub fn zero_fill_declined(&self, meta_resident: bool) {
        if meta_resident {
            self.zero_fill_declined_meta.fetch_add(1, Ordering::Relaxed);
        } else {
            self.zero_fill_declined_absent
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A request's slot was released, `ns` after it was created.
    pub fn completed(&self, ns: u64) {
        self.lat_ns_sum.fetch_add(ns, Ordering::Relaxed);
        self.lat_ns_max.fetch_max(ns, Ordering::Relaxed);
        self.lat_count.fetch_add(1, Ordering::Relaxed);
        self.lat_bucket[bucket(ns / 1000, &LAT_US)].fetch_add(1, Ordering::Relaxed);
    }

    /// The three segments of a released request, in nanoseconds since its creation. `submitted` and
    /// `first_compl` are absolute offsets from creation; `total` is its whole age.
    pub fn completed_split(&self, submitted: u64, first_compl: u64, total: u64) {
        let wait = first_compl.saturating_sub(submitted);
        let spread = total.saturating_sub(first_compl);
        self.pre_ns_sum.fetch_add(submitted, Ordering::Relaxed);
        self.pre_ns_max.fetch_max(submitted, Ordering::Relaxed);
        self.wait_ns_sum.fetch_add(wait, Ordering::Relaxed);
        self.wait_ns_max.fetch_max(wait, Ordering::Relaxed);
        self.spread_ns_sum.fetch_add(spread, Ordering::Relaxed);
        self.spread_ns_max.fetch_max(spread, Ordering::Relaxed);
        self.split_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Time spent installing one completion's pages.
    pub fn installed_ns(&self, ns: u64) {
        self.install_ns_sum.fetch_add(ns, Ordering::Relaxed);
        self.install_ns_max.fetch_max(ns, Ordering::Relaxed);
    }

    /// One page-data completion, after its pages have been installed.
    pub fn completion(
        &self,
        delivered: usize,
        installed: usize,
        dup: usize,
        dup_large: usize,
        merged: usize,
    ) {
        self.duplicate_large
            .fetch_add(dup_large as u64, Ordering::Relaxed);
        self.completions.fetch_add(1, Ordering::Relaxed);
        if dup > 0 {
            self.completions_with_dup.fetch_add(1, Ordering::Relaxed);
        }
        self.delivered
            .fetch_add(delivered as u64, Ordering::Relaxed);
        self.installed
            .fetch_add(installed as u64, Ordering::Relaxed);
        self.duplicate.fetch_add(dup as u64, Ordering::Relaxed);
        self.merged_large
            .fetch_add(merged as u64, Ordering::Relaxed);
    }
}

/// The subset of these counters that is exported to userspace; see
/// [`twizzler_abi::syscall::KernelStats`].
pub struct PagerTotals {
    pub requests: u64,
    pub pages_requested: u64,
    pub pages_delivered: u64,
    pub pages_installed: u64,
    pub completions: u64,
}

pub fn totals() -> PagerTotals {
    let p = &PAGER_PROFILE;
    PagerTotals {
        requests: p.submitted.load(Ordering::Relaxed),
        pages_requested: p.pages_requested.load(Ordering::Relaxed),
        pages_delivered: p.delivered.load(Ordering::Relaxed),
        pages_installed: p.installed.load(Ordering::Relaxed),
        completions: p.completions.load(Ordering::Relaxed),
    }
}

pub fn print_pager_profile() {
    let p = &PAGER_PROFILE;
    // Before the early return: a boot where nothing was submitted still did lookups, and the
    // in-kernel hit path is half of what this measures.
    lookupstats::print();
    let submitted = p.submitted.load(Ordering::Relaxed);
    if submitted == 0 {
        return;
    }
    let pages = p.pages_requested.load(Ordering::Relaxed);
    let page_reqs = p.page_reqs.load(Ordering::Relaxed).max(1);
    logln!(
        "== pager profile: {} requests submitted, {} coalesced, {} refused for want of a slot ==",
        submitted,
        p.coalesced.load(Ordering::Relaxed),
        p.no_slot.load(Ordering::Relaxed),
    );
    logln!(
        "  pages asked for: {} over {} page-data requests ({} per request)",
        pages,
        p.page_reqs.load(Ordering::Relaxed),
        pages / page_reqs,
    );
    logln!(
        "  not asked for again: {} requests waited on one covering them, {} on one covering only \
         their required pages, {} narrowed against requests in flight ({} speculative pages \
         dropped)",
        p.covered.load(Ordering::Relaxed),
        p.covered_required.load(Ordering::Relaxed),
        p.narrowed.load(Ordering::Relaxed),
        p.narrowed_pages.load(Ordering::Relaxed),
    );

    super::boost::print_stats();

    if let Some(rs) = super::queues::completion_recv_stats() {
        // `parks` is what the spin was supposed to avoid and `spins` is what it cost trying: a
        // parks-to-recvs ratio near one means the producer answers one request at a time and the
        // spin can only ever be paid, never won, which is what the budget adapts away from.
        logln!(
            "  completion queue: {} receives, {} parked ({} spin iterations spent, budget now {})",
            rs.recvs,
            rs.parks,
            rs.spins,
            rs.budget,
        );
    }

    let mgr_n = p.mgr_lock_count.load(Ordering::Relaxed).max(1);
    let idmap_n = p.idmap_lock_count.load(Ordering::Relaxed).max(1);
    logln!(
        "  request-path locks: mgr {} acquisitions, mean {} ns, max {} us, {} contended; idmap {} \
         acquisitions, mean {} ns, max {} us, {} contended",
        p.mgr_lock_count.load(Ordering::Relaxed),
        p.mgr_lock_ns.load(Ordering::Relaxed) / mgr_n,
        p.mgr_lock_max_ns.load(Ordering::Relaxed) / 1000,
        p.mgr_lock_slow.load(Ordering::Relaxed),
        p.idmap_lock_count.load(Ordering::Relaxed),
        p.idmap_lock_ns.load(Ordering::Relaxed) / idmap_n,
        p.idmap_lock_max_ns.load(Ordering::Relaxed) / 1000,
        p.idmap_lock_slow.load(Ordering::Relaxed),
    );

    logln!(
        "  fulfillment: {} requests done before release, {} completed by the DONE flag alone, {} \
         asked-for pages never delivered",
        p.fulfilled_by_count.load(Ordering::Relaxed),
        p.fulfilled_by_flag.load(Ordering::Relaxed),
        p.unfulfilled_pages.load(Ordering::Relaxed),
    );

    logln!(
        "  eof clamp: {} widenings trimmed ({} pages), declined {} for no stated length, {} for \
         asking past it",
        p.eof_clamped.load(Ordering::Relaxed),
        p.eof_clamped_pages.load(Ordering::Relaxed),
        p.eof_no_len.load(Ordering::Relaxed),
        p.eof_past_end.load(Ordering::Relaxed),
    );

    logln!(
        "  speculative: {} requests reached submit for {} pages, {} sent nothing (all present)",
        p.spec_reqs.load(Ordering::Relaxed),
        p.spec_pages.load(Ordering::Relaxed),
        p.spec_skipped.load(Ordering::Relaxed),
    );

    logln!(
        "  zero-fill past EOF: {} faults served ({} pages), declined {} into metadata, {} for no \
         resident meta page",
        p.zero_fill_reqs.load(Ordering::Relaxed),
        p.zero_fill_pages.load(Ordering::Relaxed),
        p.zero_fill_declined_meta.load(Ordering::Relaxed),
        p.zero_fill_declined_absent.load(Ordering::Relaxed),
    );

    let ap = p.asked_present_reqs.load(Ordering::Relaxed);
    if ap > 0 {
        logln!(
            "  trimmed pages already held: {} requests, {} pages, {} at most in one request",
            ap,
            p.asked_present_pages.load(Ordering::Relaxed),
            p.asked_present_max.load(Ordering::Relaxed),
        );
    }

    const DEPTH_NAMES: [&str; NR_DEPTH] = ["<=1", "<=2", "<=4", "<=8", "<=16", ">16"];
    log!("  outstanding at submit:");
    for (i, name) in DEPTH_NAMES.iter().enumerate() {
        log!(" {} {}", name, p.depth[i].load(Ordering::Relaxed));
    }
    logln!("  (max {} at once)", p.max_depth.load(Ordering::Relaxed));

    let lat_count = p.lat_count.load(Ordering::Relaxed);
    if lat_count > 0 {
        logln!(
            "  request latency: {} done, mean {} us, max {} us  [<100us {} <1ms {} <10ms {} >= {}]",
            lat_count,
            p.lat_ns_sum.load(Ordering::Relaxed) / lat_count / 1000,
            p.lat_ns_max.load(Ordering::Relaxed) / 1000,
            p.lat_bucket[0].load(Ordering::Relaxed),
            p.lat_bucket[1].load(Ordering::Relaxed),
            p.lat_bucket[2].load(Ordering::Relaxed),
            p.lat_bucket[3].load(Ordering::Relaxed),
        );
    }

    let split = p.split_count.load(Ordering::Relaxed);
    if split > 0 {
        logln!(
            "  latency split over {} requests, mean us: submit {} / pager {} / spread {}  \
             (max us: {} / {} / {})",
            split,
            p.pre_ns_sum.load(Ordering::Relaxed) / split / 1000,
            p.wait_ns_sum.load(Ordering::Relaxed) / split / 1000,
            p.spread_ns_sum.load(Ordering::Relaxed) / split / 1000,
            p.pre_ns_max.load(Ordering::Relaxed) / 1000,
            p.wait_ns_max.load(Ordering::Relaxed) / 1000,
            p.spread_ns_max.load(Ordering::Relaxed) / 1000,
        );
        let completions = p.completions.load(Ordering::Relaxed).max(1);
        logln!(
            "  of the spread, installing pages: {} us total, {} us per completion, {} us max",
            p.install_ns_sum.load(Ordering::Relaxed) / 1000,
            p.install_ns_sum.load(Ordering::Relaxed) / completions / 1000,
            p.install_ns_max.load(Ordering::Relaxed) / 1000,
        );
    }

    let delivered = p.delivered.load(Ordering::Relaxed);
    let dup = p.duplicate.load(Ordering::Relaxed);
    logln!(
        "  page data: {} completions ({} carrying a duplicate), {} pages delivered, {} installed, \
         {} already present, {} merged large",
        p.completions.load(Ordering::Relaxed),
        p.completions_with_dup.load(Ordering::Relaxed),
        delivered,
        p.installed.load(Ordering::Relaxed),
        dup,
        p.merged_large.load(Ordering::Relaxed),
    );
    if delivered > 0 {
        let dup_large = p.duplicate_large.load(Ordering::Relaxed);
        logln!(
            "  wasted transfer: {}permille ({} of the duplicates were under a large page, {} \
             under a 4KiB one)",
            (dup * 1000) / delivered,
            dup_large,
            dup - dup_large,
        );
    }
}
