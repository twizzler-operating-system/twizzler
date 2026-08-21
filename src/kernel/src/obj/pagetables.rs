use alloc::vec::Vec;

use itertools::Itertools;
use twizzler_abi::device::CacheType;
use twizzler_rt_abi::error::TwzError;

use crate::{
    arch::{
        PhysAddr, VirtAddr,
        context::ArchContextTarget,
        memory::pagetables::{ArchTlbMgr, PendingShootdown},
    },
    memory::{
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS, get_frame, min_level_for_len},
        pagetables::{
            Consistency, ContiguousProvider, DeferredUnmappingOps, MapInfo, MapReader, Mapper,
            MappingCursor, MappingSettings, Table, TlbOrigin,
        },
        tracker::{
            FrameAllocFlags, FrameAllocator, alloc_frame, allocprofile, free_frame,
            take_or_new_frame_allocator,
        },
    },
    obj::{Object, ObjectRef, PageNumber},
};

/// Kept at 8: raising it to 16 was measured (`widen2-a`/`widen2-b`) and changed nothing. The same
/// 18 objects latch either way -- they just latch on `MAX_INVLS` instead, since they exceed both
/// limits. See unmap.md.
const MAX_INVL_TARGETS: usize = 8;
const MAX_INVLS: usize = 4;
/// Capacity of the membership set. See [ObjectPageTable::members].
const MAX_MEMBERS: usize = 32;

/// Precharge the page-table frames a mapping will *actually* need, rather than the most it could
/// ever need.
///
/// `MappingCursor::max_number_new_tables` answers from geometry alone -- one frame per level,
/// whatever is already installed -- so a 4 KiB `map_page` always asks for `top_level()` frames.
/// Measured on `page_fault_zero_fill`: **one `Table::populate` per 205 `map_page` calls**
/// (`populated=8,308` against `calls=1,706,024`), so 99.5% of those requests are borrow-and-return.
///
/// With the per-cpu pool knobs on that stopped being nearly free: a per-operation allocator is
/// built fresh, so the request reaches `precharge`, which reserves and then unparks. The span went
/// 35 ns -> 620 ns across the flip. Skipping the call entirely when nothing is needed is what
/// removes it, and [`Table::tables_needed`] is what decides.
///
/// The failure that matters is *under*-counting: a short precharge sends `try_allocate` to the
/// global allocator without `WAIT_OK` while the object's page-table lock is held. That is exactly
/// what `avoid_alloc` reports -- `avoid-empty=` on `PERFMARK-FA` -- and it must stay 0.
const PRECHARGE_EXACT: bool = true;

/// Skip the consistency epilogue when there is nothing to invalidate and nothing to free.
///
/// See [`Consistency::is_trivial`] for what "nothing" costs without this. The enqueue itself is
/// already conditional; this is about the machinery around it.
const CONSIST_FASTPATH: bool = true;

/// Second-and-later operations parked under one page-table lock hold, merged into the batch the
/// guard will discharge. See [ObjectPageTable::park].
///
/// Measured at ~150k per boot, which is why `park` merges rather than discharging the older batch
/// inline: a lock hold runs one consistency-generating operation *per page* of a page-in or copy
/// loop, not one per hold. Under the discharge-inline design only the last park in a hold survived
/// to the guard, so on the order of (k-1)/k of all real waits stayed inside the lock -- the thing
/// the guard exists to prevent. Kept as a counter because it is the number that falsified the
/// "one operation per hold" assumption, and would catch a future change that reintroduced it.
pub mod merged_parks {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    pub fn record() {
        N.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count() -> usize {
        N.load(Ordering::Relaxed)
    }
}

/// Where a single `map_page` call's time goes, bracketed so that the *gap* is measurable.
///
/// Separate from [`crate::memory::tracker::allocprofile`] deliberately. That module's `counters!`
/// list is indexed positionally by `perfmark`, it belongs to the frame-allocator work, and its
/// `TIME_ALLOCS` gate can be flipped for an allocator arm -- probes gated on someone else's const
/// go live inside their measurement. This has its own switch and its own snapshot.
///
/// The design rule this follows: **bracket the gap, not the pieces already suspected.** `BODY`
/// spans the whole function body, so `FILL_MAP_NS - BODY` is prologue/epilogue and the call
/// itself, and `BODY - sum(spans)` is time between probes rather than inside any of them. The
/// prior split of this function reported prep/walk/consist summing to 1,164 ns against a 1,712 ns
/// whole and left 548 ns attributed to nothing; that residual is the thing being measured here,
/// so it must not be inferred from a subtraction of numbers taken by a different instrument.
pub mod mapprobe {
    use core::sync::atomic::{AtomicU64, Ordering};

    /// Off in the committed tree. Costs ~10 clock-read pairs per `map_page`.
    pub const MAP_PROBE: bool = false;

    macro_rules! counters {
        ($($name:ident),* $(,)?) => {
            $(pub static $name: AtomicU64 = AtomicU64::new(0);)*
            pub const NAMES: &[&str] = &[$(stringify!($name)),*];
            pub const NR: usize = NAMES.len();
            pub fn snapshot() -> [u64; NR] {
                [$($name.load(Ordering::Relaxed)),*]
            }
        };
    }

    counters!(
        // `map_page`, one record each per call.
        CALLS,
        BODY_NS,
        CONS_NEW_NS,
        TAKE_FA_NS,
        PRECHARGE_NS,
        PROV_NS,
        WALK_NS,
        CONSIST_NS,
        DROP_FA_NS,
        DROP_PHYS_NS,
        // `run_consistency`, split. Counted separately because `map_page` is not its only caller
        // and the reading is only clean in a window where `RC_CALLS == CALLS`.
        RC_CALLS,
        RC_SEND_NS,
        RC_RESET_NS,
        RC_PARK_NS,
        // Page tables actually created by `Table::populate`. This is the denominator that says
        // whether `map_page`'s per-page precharge buys anything: it asks for
        // `max_number_new_tables` (2 on amd64 object tables) on every call, and a sequential fault
        // run needs a new leaf table once per 512 pages.
        POPULATED,
        // Back-to-back start/record, i.e. the floor under every span above.
        PROBE_NS,
        // The *perturbation*, which `PROBE_NS` structurally cannot see: an outer bracket around a
        // complete inner probe. `PROBE_OUTER_NS - PROBE_NS` is what one `record` call costs the
        // bracket that encloses it -- the term that lands in `gap` and in nobody's span. Measured
        // rather than assumed, because assuming it is how the previous split of this function
        // ended up attributing 548 ns to nothing.
        PROBE_OUTER_NS,
        // Appended, not inserted -- `perfmark` indexes this snapshot positionally and putting this
        // beside `RC_*` where it reads better shifted `PROBE_NS` and `PROBE_OUTER_NS` by one,
        // which is the same silent break the `allocprofile` list carries a warning about.
        //
        // `run_consistency` calls that had nothing to invalidate and nothing to free, i.e. took
        // the fast path. Read against `RC_CALLS`: on this bench it should be ~all of them, and a
        // build where it is not is one where the epilogue is doing real work.
        RC_TRIVIAL,
    );

    pub fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }

    /// Counters accumulate **raw ticks**, converted once at print time.
    ///
    /// This is not a micro-optimization, it is what makes the residual measurable. The existing
    /// probes (`allocprofile::record`, `fault::record_stage`) do
    /// `(Instant::now() - start).into()` and then `as_nanos()`: a u128 multiply plus a u128
    /// division and modulo by 10^15, then a `Duration` round trip -- **all of it after the second
    /// clock read**, so it is charged to whatever bracket encloses the probe and to none of the
    /// spans inside it. Their `PROBE_NS` floor cannot see this: it reports the *interval* between
    /// two back-to-back readings (7.6 ns), not the *perturbation* the probe adds, which happens
    /// once the interval has already closed.
    ///
    /// That is almost certainly what the 255 ns of `map_page` that remains unbracketed after
    /// `MAP_DROP_NS` is put back (see `zerofill.md` C2) actually is: four `record` calls inside
    /// the body, each dropping its conversion into the enclosing `FILL_MAP_NS`. Accumulating
    /// ticks here is what lets that be tested rather than argued -- with the conversion gone, the
    /// gap should collapse.
    pub fn start() -> u64 {
        if MAP_PROBE {
            crate::instant::Instant::now().raw_ticks()
        } else {
            0
        }
    }

    pub fn record(c: &AtomicU64, start: u64) {
        if !MAP_PROBE {
            return;
        }
        let now = crate::instant::Instant::now().raw_ticks();
        add(c, now.saturating_sub(start));
    }

    /// Ticks to nanoseconds, using the clock's own rate. Print path only.
    pub fn ticks_to_ns(ticks: u64) -> u64 {
        let now = crate::instant::Instant::now();
        now.ns_since_ticks(now.raw_ticks().saturating_sub(ticks))
    }

    /// One count, with no clock read, for the paths that only need a denominator.
    pub fn tick(c: &AtomicU64) {
        if !MAP_PROBE {
            return;
        }
        add(c, 1);
    }
}

/// How often an object's `invls` has overflowed -- i.e. it no longer knows every context its
/// mappings live in -- broken out by what the caller then does about it.
///
/// `Remove` is the one to watch. There, overflow makes [ObjectPageTable::remove_invalidate] return
/// without removing anything, so the object's record of where it is mapped stops shrinking. That is
/// harmless for invalidation, which only over-invalidates as a result -- but it is the *unsafe*
/// direction for anything that later treats this as authoritative membership, which is why
/// unmap.md's stage 2 cannot simply inherit the behaviour. Today the drift is silent; this makes it
/// a number.
///
/// `Invalidate` and `Send` count the two `new_full_global()` fallbacks, so this also prices what a
/// membership structure that could not overflow would retire.
pub mod invl_overflow {
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Copy)]
    pub enum Site {
        Remove = 0,
        Invalidate = 1,
        Send = 2,
    }

    const NR: usize = 3;
    static CALLS: [AtomicUsize; NR] = [const { AtomicUsize::new(0) }; NR];
    static OVER: [AtomicUsize; NR] = [const { AtomicUsize::new(0) }; NR];

    /// Distinct objects that have ever latched, and distinct objects seen latched at each site.
    /// Counted per object rather than per call, because overflow is permanent: a latched object is
    /// hit on every subsequent call, so a rate cannot distinguish "many objects overflow
    /// occasionally" from "a handful latched early and are hammered forever". Those want opposite
    /// fixes.
    static OBJECTS: AtomicUsize = AtomicUsize::new(0);
    static OBJECTS_AT: [AtomicUsize; NR] = [const { AtomicUsize::new(0) }; NR];
    /// Which limit admitted each latching object. `overflowed()` is a disjunction and the two
    /// routes are unrelated defects: the outer list filling means eight contexts accumulated, while
    /// an inner list filling means `MAX_INVLS` cursors on a single target and says nothing about
    /// how many contexts map the object. `BOTH` is kept apart rather than folded into either,
    /// since an object over both limits does not say which it crossed first.
    static LATCH_OUTER: AtomicUsize = AtomicUsize::new(0);
    static LATCH_INNER: AtomicUsize = AtomicUsize::new(0);
    static LATCH_BOTH: AtomicUsize = AtomicUsize::new(0);
    /// Live targets at the instant of latching: an object genuinely mapped into eight contexts at
    /// once against one that cycled through eight and holds two.
    static LATCH_LIVE_SUM: AtomicUsize = AtomicUsize::new(0);
    static LATCH_LIVE_MAX: AtomicUsize = AtomicUsize::new(0);
    /// How close the workload came, over every call rather than only latching ones. Without these a
    /// reading of zero latched objects cannot be told from a workload that never approached the
    /// limit -- and zero would read as good news either way.
    static MAX_LIVE: AtomicUsize = AtomicUsize::new(0);
    static MAX_LEN: AtomicUsize = AtomicUsize::new(0);

    pub fn record(site: Site, overflowed: bool) {
        CALLS[site as usize].fetch_add(1, Ordering::Relaxed);
        if overflowed {
            OVER[site as usize].fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Runs on every call, so the maxima are guarded by a relaxed load rather than taken
    /// unconditionally: `fetch_max` has no native instruction on x86_64 or aarch64 and lowers to a
    /// `lock cmpxchg` retry loop, which is a different cost class from the `fetch_add`s beside it
    /// and would put two contended RMWs per call on the TLB-invalidation path. The load-then-CAS
    /// race is benign because the maxima are monotonic and only read at shutdown: a lost update is
    /// re-attempted by the next caller to exceed it. After the first few calls neither branch is
    /// taken.
    pub fn record_shape(live: usize, len: usize) {
        if live > MAX_LIVE.load(Ordering::Relaxed) {
            MAX_LIVE.fetch_max(live, Ordering::Relaxed);
        }
        if len > MAX_LEN.load(Ordering::Relaxed) {
            MAX_LEN.fetch_max(len, Ordering::Relaxed);
        }
    }

    pub fn record_latch(live: usize, outer_full: bool, inner_full: bool) {
        OBJECTS.fetch_add(1, Ordering::Relaxed);
        LATCH_LIVE_SUM.fetch_add(live, Ordering::Relaxed);
        LATCH_LIVE_MAX.fetch_max(live, Ordering::Relaxed);
        match (outer_full, inner_full) {
            (true, true) => &LATCH_BOTH,
            (true, false) => &LATCH_OUTER,
            (false, true) => &LATCH_INNER,
            (false, false) => return,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_object_at(site: Site) {
        OBJECTS_AT[site as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// Set on the `latched` byte when an object first latches, and cleared by the one call site
    /// that can name it. Bit 7 so it cannot collide with the per-site bits.
    pub const NOTICE: u8 = 1 << 7;

    /// Names the first few latched objects, so the population can be identified rather than only
    /// counted. Bounded because the question is identity -- "is the runtime text object among
    /// these" -- not quantity, which `OBJECTS` already answers.
    ///
    /// Only the `remove_invalidate` call site can do this: `ObjectPageTable` has no back-pointer to
    /// its `Object`, and of the three sites calling `overflowed()` it is the only one in
    /// `virtmem.rs` with the object in scope. That is sufficient rather than a compromise --
    /// stage 1b measured `OBJECTS_AT` at 18/0/0, so every latch is first observed there. If that
    /// ever stops being true, `NAMED` falls short of `OBJECTS` and the gap is the signal.
    static NAMED: AtomicUsize = AtomicUsize::new(0);
    const MAX_NAMED: usize = 24;

    pub fn note_object(id: crate::obj::ObjID, live: usize, len: usize) {
        let n = NAMED.fetch_add(1, Ordering::Relaxed);
        if n < MAX_NAMED {
            emerglogln!(
                "== invls latched object {}: {} live of {} targets",
                id,
                live,
                len
            );
        }
    }

    pub fn named() -> usize {
        NAMED.load(Ordering::Relaxed)
    }

    pub fn print() {
        for (i, name) in ["remove", "invalidate", "send"].iter().enumerate() {
            emerglogln!(
                "== invls overflow ({}): {} of {} calls, {} distinct objects",
                name,
                OVER[i].load(Ordering::Relaxed),
                CALLS[i].load(Ordering::Relaxed),
                OBJECTS_AT[i].load(Ordering::Relaxed),
            );
        }
        let objects = OBJECTS.load(Ordering::Relaxed);
        emerglogln!(
            "== invls latched: {} objects (outer {}, inner {}, both {}), live at latch {}/100 mean, {} max; ever seen {} live, {} len",
            objects,
            LATCH_OUTER.load(Ordering::Relaxed),
            LATCH_INNER.load(Ordering::Relaxed),
            LATCH_BOTH.load(Ordering::Relaxed),
            if objects == 0 {
                0
            } else {
                LATCH_LIVE_SUM.load(Ordering::Relaxed) * 100 / objects
            },
            LATCH_LIVE_MAX.load(Ordering::Relaxed),
            MAX_LIVE.load(Ordering::Relaxed),
            MAX_LEN.load(Ordering::Relaxed),
        );
    }
}

/// Stage 2 bookkeeping: how big membership actually gets, how often it gives up, and whether it
/// ever disagrees with the ground truth of the existing fan-out.
///
/// `MAX_SIZE` is the number nothing else in the tree measures. Every prior attempt to size this
/// read a container's own capacity back: `live at latch` cannot exceed `MAX_INVL_TARGETS`, and the
/// census's top bucket is unbounded. This one is capped only at [MAX_MEMBERS], which is set well
/// above the 13 contexts the shared runtime is known to reach -- so a reading below 32 is a
/// measurement and a reading of 32 is a ceiling. Read it that way.
///
/// `MISSES` is the stage that cannot be skipped: it counts contexts that released a mapping while
/// *absent* from a known-complete membership set. Any non-zero value means the set cannot be
/// maintained correctly at the points chosen, and the design needs revisiting rather than patching
/// -- a stale mapping is not a crash, so nothing else would report it.
pub mod membership {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static MAX_SIZE: AtomicUsize = AtomicUsize::new(0);
    static UNKNOWN: AtomicUsize = AtomicUsize::new(0);
    static CHECKS: AtomicUsize = AtomicUsize::new(0);
    static MISSES: AtomicUsize = AtomicUsize::new(0);
    static SATURATED: AtomicUsize = AtomicUsize::new(0);

    pub fn record_size(n: usize) {
        if n > MAX_SIZE.load(Ordering::Relaxed) {
            MAX_SIZE.fetch_max(n, Ordering::Relaxed);
        }
    }

    pub fn record_unknown() {
        UNKNOWN.fetch_add(1, Ordering::Relaxed);
    }

    /// Times a target could not be recorded because the set was full. Non-zero means `MAX_SIZE` has
    /// hit [MAX_MEMBERS] and is therefore a ceiling again, not a measurement -- which is the only
    /// way to tell those apart from the number itself.
    pub fn record_saturated() {
        SATURATED.fetch_add(1, Ordering::Relaxed);
    }

    /// One context that released a mapping, against what membership claimed.
    pub fn record_check(present: bool) {
        CHECKS.fetch_add(1, Ordering::Relaxed);
        if !present {
            MISSES.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn print() {
        let misses = MISSES.load(Ordering::Relaxed);
        emerglogln!(
            "== membership: max {} of {} ({}), {} objects unknown, {} checks, {} MISSES{}",
            MAX_SIZE.load(Ordering::Relaxed),
            super::MAX_MEMBERS,
            // Says outright whether the maximum is data or a ceiling, rather than leaving the
            // reader to infer it from the number's proximity to the bound -- which is the
            // inference that went wrong last time.
            if SATURATED.load(Ordering::Relaxed) == 0 {
                "measured"
            } else {
                "CEILING"
            },
            UNKNOWN.load(Ordering::Relaxed),
            CHECKS.load(Ordering::Relaxed),
            misses,
            if misses == 0 {
                ""
            } else {
                "  <-- SET IS WRONG"
            },
        );
    }
}

/// What became of a run of pages the pager delivered.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct InstallTally {
    pub installed: usize,
    /// Pages the object already held, so the delivery was wasted.
    pub dup: usize,
    /// Of `dup`, those a large page already covers. Worth counting apart from the small case: one
    /// overlapping request that loses the race to a merge has every one of its pages in that
    /// 2 MiB region rejected, so a handful of merges can account for a great many duplicates.
    pub dup_large: usize,
}

pub struct ObjectPageTable {
    mapper: Mapper,
    invls: heapless::Vec<
        (ArchContextTarget, heapless::Vec<MappingCursor, MAX_INVLS>),
        MAX_INVL_TARGETS,
    >,
    /// Which contexts have this object mapped -- stage 2 of unmap.md, maintained but not yet used.
    ///
    /// Deliberately *separate* from `invls` rather than derived from it. `invls` carries two facts
    /// at once, "which contexts map this" and "which cursors to invalidate", in one bounded
    /// structure; incompleteness in either therefore poisons both, which is why overflow there has
    /// to be permanent. Membership is one entry per context and nothing else.
    ///
    /// Sized well above `MAX_INVL_TARGETS`, and that is the point: the shared runtime objects
    /// reach 13 contexts (one per compartment, measured -- see unmap.md), so an 8-entry bound
    /// is exceeded by arithmetic rather than by load. 32 leaves room for a machine with more
    /// compartments before `unknown` is reached.
    members: heapless::Vec<ArchContextTarget, MAX_MEMBERS>,
    /// Set when membership can no longer be trusted to be complete, and never cleared. Biases
    /// every discrepancy toward over-inclusion: a consumer seeing this must fall back to
    /// iterating every context, which is exactly today's behaviour, rather than trusting a
    /// short list.
    members_unknown: bool,
    map_count: usize,
    /// Work that must happen after this object's page-table lock is released: waiting for the
    /// shootdowns issued under it, and then freeing the frames those shootdowns protect.
    ///
    /// Parked here rather than run inline because the wait dominates the lock hold -- a median
    /// 90 ms per boot of object-origin wait time, all of it with this mutex held (see TLB.md) --
    /// and none of it needs the lock. [PtGuard] takes it and runs it after unlocking; living
    /// behind the same mutex as everything else here is what makes that handoff safe.
    deferred: Option<DeferredUnmappingOps>,
    /// Which call sites have observed this object over its `invls` limits, one bit per
    /// [invl_overflow::Site]. Never cleared, because the condition is permanent (see
    /// [Self::overflowed]) -- so this is a property of the object rather than of a call, and the
    /// counters keyed on it count objects rather than rates.
    ///
    /// `Cell` because `overflowed()` takes `&self` for `send_consistency`'s benefit. The struct
    /// lives behind the object's page-table mutex, and `Mutex<T>: Sync` requires only `T: Send`
    /// (mutex.rs), which `Cell<u8>` satisfies.
    latched: core::cell::Cell<u8>,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct FindFrameFlags: u32 {
        const ALLOW_NOT_ZEROED = (1 << 0);
        const WRITE = (1 << 1);
        const POPULATE = (1 << 2);
    }
}

impl Drop for ObjectPageTable {
    fn drop(&mut self) {
        let mut consist = Consistency::new_object_tables();
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), self.max_len());
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let _ = self.mapper.unmap(cursor, &mut consist, &mut fa, &mut None);
        self.run_consistency(consist);
        // No guard is going to come along and discharge this -- the object is going away -- so the
        // parked work has to run here, before the root frame below is freed.
        if let Some(ops) = self.deferred.take() {
            ops.run_all();
        }
        let root_frame = get_frame(self.mapper.root_address()).expect("root frame should exist");
        root_frame.set_pt(false);
        if root_frame.dec_refcount() == 0 {
            free_frame(root_frame);
        }
    }
}

#[derive(Default, Debug)]
pub struct DirtyList {
    pages: Vec<(PageNumber, PhysAddr, usize)>,
    frames: Vec<FrameRef>,
}

impl DirtyList {
    pub fn pages(&self) -> &Vec<(PageNumber, PhysAddr, usize)> {
        &self.pages
    }

    pub fn frames(&self) -> &Vec<FrameRef> {
        &self.frames
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

impl Drop for DirtyList {
    fn drop(&mut self) {
        for frame in self.frames.drain(..) {
            if frame.dec_refcount() == 0 {
                free_frame(frame);
            }
        }
    }
}

impl ObjectPageTable {
    pub fn new() -> Self {
        let frame = alloc_frame(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK,
        );
        crate::memory::frame::ensure_pt_zeroed(frame, "object page table root");
        frame.set_pt(true);
        frame.inc_refcount();
        let mut mapper = Mapper::new(frame.start_address());
        mapper.set_start_level(Self::top_level());
        Self {
            mapper,
            invls: heapless::Vec::new(),
            members: heapless::Vec::new(),
            members_unknown: false,
            map_count: 0,
            deferred: None,
            latched: core::cell::Cell::new(0),
        }
    }

    /// Hand post-unlock work to [PtGuard].
    ///
    /// Two operations under one lock hold is rare, so rather than merging frame lists the older one
    /// is discharged inline here -- which is exactly the behaviour every one of these call sites
    /// had before parking existed.
    ///
    /// But that inline discharge runs the shootdown wait and the frame frees back *inside* the hold
    /// this whole change exists to shorten, so the improvement is conditional on there being one
    /// consistency-generating operation per lock hold. Counted rather than assumed: zero per boot
    /// proves the fast path, and anything else names the site, instead of showing up later as an
    /// unexplained number in the hold-time instrumentation.
    fn park(&mut self, ops: DeferredUnmappingOps) {
        match self.deferred.as_mut() {
            Some(prev) => {
                prev.absorb(ops);
                merged_parks::record();
            }
            None => self.deferred = Some(ops),
        }
    }

    pub fn take_deferred(&mut self) -> Option<DeferredUnmappingOps> {
        self.deferred.take()
    }

    pub fn top_level() -> usize {
        Table::top_level() - 1
    }

    pub fn map_count(&self) -> usize {
        self.map_count
    }

    /// The table a context's page tables point at for this object, i.e. what [Mapper::object_map]
    /// installs and what unmapping releases. `None` if this object has never been mapped. Used to
    /// tell an unmap of *our* mapping from an unmap of whatever else ended up at that address.
    pub fn context_table_addr(&self) -> Option<PhysAddr> {
        self.mapper.peek_table_addr(Self::top_level() - 1)
    }

    pub fn inc_map_count(&mut self) {
        self.map_count += 1;
    }

    pub fn dec_map_count(&mut self) {
        assert!(self.map_count > 0, "map count cannot be negative");
        self.map_count -= 1;
    }

    /// Record that `target` maps this object. Idempotent, and biased to over-inclusion: if the set
    /// is full it goes `unknown` rather than silently dropping the target, which is the failure
    /// `invls` has and the reason membership is a separate structure.
    fn add_member(&mut self, target: ArchContextTarget) {
        if self.members.contains(&target) {
            return;
        }
        // Keep counting past `unknown`. An earlier version returned before this and so stopped
        // growing the set the moment the object latched -- which made the reported maximum track
        // `MAX_INVL_TARGETS` rather than the workload, and it was published as a real measurement.
        // Membership is not *used* once unknown, so continuing to fill it costs nothing and is what
        // makes the maximum bounded only by `MAX_MEMBERS`.
        if self.members.push(target).is_err() {
            if !self.members_unknown {
                self.members_unknown = true;
                membership::record_unknown();
            }
            membership::record_saturated();
            return;
        }
        membership::record_size(self.members.len());
    }

    /// Drop `target` from membership, but only when it can be *proved* to hold no more mappings of
    /// this object -- which means the cursor list for it is authoritative. Once `invls` has
    /// overflowed it is not, so membership goes `unknown` instead of guessing.
    ///
    /// Removing on a lossy cursor list is the one move that would make this unsafe: a context still
    /// holding a mapping would drop out of the set, and a consumer iterating membership would skip
    /// its unmap. Over-inclusion costs a wasted acquisition; under-inclusion is a stale mapping.
    fn drop_member_if_drained(&mut self, target: ArchContextTarget) {
        if self.members_unknown {
            return;
        }
        let drained = self
            .invls
            .iter()
            .find(|(t, _)| *t == target)
            .is_none_or(|(_, maps)| maps.is_empty());
        if drained && let Some(pos) = self.members.iter().position(|t| *t == target) {
            self.members.swap_remove(pos);
        }
    }

    /// Whether membership is complete enough to iterate instead of every attached context. Stage 3
    /// is what will consult this; stage 2 only maintains it.
    pub fn members(&self) -> Option<&[ArchContextTarget]> {
        (!self.members_unknown).then(|| self.members.as_slice())
    }

    pub fn add_invalidate(&mut self, target: ArchContextTarget, cursor: MappingCursor) {
        self.add_member(target);
        if let Some((_, maps)) = self.invls.iter_mut().find(|(t, _)| *t == target) {
            if !maps.iter().contains(&cursor) {
                let _ = maps.push(cursor);
            }
        } else {
            let mut maps = heapless::Vec::new();
            let _ = maps.push(cursor);
            let _ = self.invls.push((target, maps));
        }
    }

    /// Whether this object has lost track of where its mappings live: either the target list is
    /// full, or some target's cursor list is. Bounded by construction (`MAX_INVL_TARGETS`,
    /// `MAX_INVLS`), and `add_invalidate` drops silently once either fills.
    ///
    /// One method rather than the predicate written out at each of the three call sites, so that
    /// every ask is counted and the three cannot drift apart.
    /// The condition is **permanent** once true: `invls` is append-only -- a `(target, maps)` pair
    /// is never removed, even after its cursors drain -- and `remove_invalidate` returns early
    /// while overflowed, so the inner lists cannot drain either. Deliberate rather than an
    /// oversight: additions were dropped while full, so the list is incomplete, and letting it
    /// drain back under the limit would resume precise invalidation with a context untracked.
    /// See unmap.md.
    fn overflowed(&self, site: invl_overflow::Site) -> bool {
        // One walk yields all three facts; this runs on every call, not only latching ones.
        let len = self.invls.len();
        let (mut live, mut inner_full) = (0usize, false);
        for (_, maps) in self.invls.iter() {
            live += !maps.is_empty() as usize;
            inner_full |= maps.is_full();
        }
        let outer_full = self.invls.is_full();
        let overflowed = outer_full || inner_full;

        invl_overflow::record(site, overflowed);
        invl_overflow::record_shape(live, len);
        if overflowed {
            let seen = self.latched.get();
            let bit = 1u8 << (site as u8);
            if seen == 0 {
                invl_overflow::record_latch(live, outer_full, inner_full);
                self.latched.set(invl_overflow::NOTICE);
            }
            if seen & bit == 0 {
                self.latched.set(self.latched.get() | bit);
                invl_overflow::record_object_at(site);
            }
        }
        overflowed
    }

    /// True exactly once per object, on the first latch, so the one call site that knows this
    /// object's id can name it. Safe to read `invls` after the `remove_invalidate` that set it:
    /// that call returns early while overflowed, so it mutates nothing.
    pub fn take_latch_notice(&self) -> bool {
        let seen = self.latched.get();
        if seen & invl_overflow::NOTICE != 0 {
            self.latched.set(seen & !invl_overflow::NOTICE);
            true
        } else {
            false
        }
    }

    /// Targets whose cursor list is non-empty, against the number of slots occupied. Both are
    /// needed to tell a widely-shared object from one that cycled: see unmap.md.
    pub fn invls_live(&self) -> usize {
        self.invls
            .iter()
            .filter(|(_, maps)| !maps.is_empty())
            .count()
    }

    pub fn invls_len(&self) -> usize {
        self.invls.len()
    }

    pub fn remove_invalidate(&mut self, target: ArchContextTarget, cursor: MappingCursor) {
        if self.overflowed(invl_overflow::Site::Remove) {
            // We might have hit the limit. Membership cannot be maintained past this point either:
            // additions were dropped while full, so the cursor list can no longer prove a target
            // has drained. Say so rather than letting the set quietly go stale.
            if !self.members_unknown {
                self.members_unknown = true;
                membership::record_unknown();
            }
            return;
        }
        if let Some((_, maps)) = self.invls.iter_mut().find(|(t, _)| *t == target) {
            if let Some(pos) = maps.iter().position(|c| c.start() == cursor.start()) {
                maps.swap_remove(pos);
            }
        }
        self.drop_member_if_drained(target);
    }

    pub fn max_len(&self) -> usize {
        PHYS_LEVEL_LAYOUTS[self.mapper.start_level()].size()
    }

    pub fn invalidate(&mut self, offset: u64, len: usize) {
        log::trace!(
            "invalidating offset {:x} len {:x} (max len {:x}) {} {} {}",
            offset,
            len,
            self.max_len(),
            self.invls.is_empty(),
            self.invls.is_full(),
            self.invls.iter().any(|(_, maps)| maps.is_full()),
        );
        if self.invls.is_empty() {
            return;
        }
        if self.overflowed(invl_overflow::Site::Invalidate) {
            let mut tlb = ArchTlbMgr::new_full_global();
            tlb.set_origin(TlbOrigin::Object);
            tlb.finish();
            return;
        }
        // Same shape as send_consistency: send for every context, then wait once, rather than a
        // complete IPI-and-wait round per context with the object's page-table lock held.
        let mut pending = PendingShootdown::none();
        for (target, maps) in self.invls.iter() {
            if maps.is_empty() {
                continue;
            }

            let mut tlb = ArchTlbMgr::new(*target);
            tlb.set_origin(TlbOrigin::Object);
            for map in maps.iter() {
                if map.start().is_kernel() {
                    // The kernel half's page tables are shared by every context, so these
                    // translations can be cached under any PCID -- and a targeted invlpg reaches
                    // only the executing cpu's current one. Nothing short of the PGE toggle behind
                    // a full+global batch covers them, which is what ArchContext::lock_with_consist
                    // already does for every other kernel-range mapping change.
                    tlb.set_full_global();
                    continue;
                }
                let mut len = map.remaining().min(len);
                let addr = match map.start().offset(offset as usize) {
                    Ok(addr) => addr,
                    Err(_) => {
                        len = self.max_len();
                        map.start()
                    }
                };
                tlb.enqueue(
                    addr,
                    false,
                    true,
                    min_level_for_len(len).unwrap_or(self.mapper.start_level()),
                );
            }
            pending.absorb(tlb.finish_send());
        }
        self.park(DeferredUnmappingOps::from_pending(pending));
    }

    pub fn invalidate_page(&mut self, pn: PageNumber) {
        let offset = pn.as_byte_offset() as u64;
        let len = PageNumber::PAGE_SIZE;
        self.invalidate(offset, len);
    }

    pub fn invalidate_full(&mut self) {
        self.invalidate(0, self.max_len());
    }

    pub fn run_consistency2(&mut self, mut consist: Consistency, other: &Self) {
        // Both objects get sent to. They did not before: `do_run_consistency` reset the accumulated
        // invalidations at the end, so the second call always found nothing pending and `other`'s
        // contexts were never invalidated at all -- visible in the old code as a `(None, Some(_))`
        // arm that could not be reached. The reset now happens once, after both.
        let mut pending = self.send_consistency(&mut consist);
        pending.absorb(other.send_consistency(&mut consist));
        consist.tlb_mut().reset();
        consist.set_pending(pending);
        let ops = consist.into_deferred();
        self.park(ops);
    }

    pub fn run_consistency(&mut self, mut consist: Consistency) {
        mapprobe::tick(&mapprobe::RC_CALLS);
        if CONSIST_FASTPATH && consist.is_trivial() {
            // Dropping `consist` here still flushes any dirty cache line through
            // `ArchCacheLineMgr`'s Drop, which is the one thing that must not be skipped. The
            // `DeferredUnmappingOps` this would otherwise park is empty, and both `take_deferred`
            // callers only ever `run_all()` it, so not parking it is indistinguishable.
            mapprobe::tick(&mapprobe::RC_TRIVIAL);
            return;
        }
        let t_m = mapprobe::start();
        let pending = self.send_consistency(&mut consist);
        mapprobe::record(&mapprobe::RC_SEND_NS, t_m);
        let t_m = mapprobe::start();
        consist.tlb_mut().reset();
        mapprobe::record(&mapprobe::RC_RESET_NS, t_m);
        let t_m = mapprobe::start();
        consist.set_pending(pending);
        let ops = consist.into_deferred();
        self.park(ops);
        mapprobe::record(&mapprobe::RC_PARK_NS, t_m);
    }

    /// Send the accumulated invalidations to every context this object is mapped into -- one
    /// shootdown per context -- and return their combined obligation, unwaited.
    ///
    /// One per context rather than one merged across all of them, because `ArchTlbMgr::merge` on
    /// two different `target_cr3`s has no precise common representation and degrades to full *and*
    /// global. Global is the expensive word: `should_target` returns true for every processor when
    /// it is set, which defeats the PCID revocation that normally reduces the target set to zero or
    /// one, and every receiver then does a CR4.PGE toggle and a full flush. Measured at ~2200 of
    /// those per boot against the arch mapper's ~320 (see TLB.md). Sending all of them before
    /// waiting for any is what keeps the precise version from costing N serial rounds instead.
    fn send_consistency(&self, consist: &mut Consistency) -> PendingShootdown {
        if !consist.tlb().has_pending() {
            return PendingShootdown::none();
        }
        // `add_invalidate` drops silently once its bounded lists fill, so past MAX_INVL_TARGETS
        // contexts (or MAX_INVLS cursors within one) this object no longer knows where all of its
        // mappings live. Retargeting precisely would then reach only the contexts that happened to
        // fit and skip the rest entirely -- the same reason `invalidate` gives up and goes global.
        let overflowed = self.overflowed(invl_overflow::Site::Send);
        if consist.tlb().is_full() || overflowed {
            let mut tlb = ArchTlbMgr::new_full_global();
            tlb.set_origin(TlbOrigin::Object);
            return tlb.finish_send();
        }

        let mut pending = PendingShootdown::none();
        for (target, maps) in self.invls.iter() {
            if maps.is_empty() {
                continue;
            }

            consist.tlb_mut().set_target(*target);

            // Merging within one target stays precise -- same `target_cr3` -- so the per-context
            // send still covers all of that context's cursors in one round.
            let mut per_target: Option<ArchTlbMgr> = None;
            for map in maps.iter() {
                let mut tlb = consist.tlb().apply_offset_from_map(map);
                // See Self::invalidate: a kernel-range mapping is visible under every PCID, so
                // precise invalidation cannot reach all of its copies. Now this makes only its own
                // context's send global rather than poisoning every other context's too.
                if map.start().is_kernel() {
                    tlb.set_full_global();
                }

                match per_target {
                    Some(ref mut acc) => acc.merge(tlb),
                    None => per_target = Some(tlb),
                }
            }
            if let Some(mut tlb) = per_target {
                pending.absorb(tlb.finish_send());
            }
        }
        pending
    }

    pub fn map_page(&mut self, offset: u64, page: FrameRef) -> Result<(), TwzError> {
        // Raw counters, not fault stages: an earlier split of this function with `record_stage`
        // put 800 ns in the three spans while the call as a whole measured 35 us, and the two
        // instruments disagreeing is itself the thing to rule out. These are the same probe the
        // caller times the whole call with.
        // `mapprobe` brackets the *whole* body as well as each piece, so the residual is measured
        // rather than inferred: see [`mapprobe`]. The `allocprofile` records below are the frame
        // allocator work's own instrument and stay where they are.
        let t_body = mapprobe::start();
        mapprobe::tick(&mapprobe::CALLS);
        let t_probe_outer = mapprobe::start();
        let t_probe = mapprobe::start();
        mapprobe::record(&mapprobe::PROBE_NS, t_probe);
        mapprobe::record(&mapprobe::PROBE_OUTER_NS, t_probe_outer);

        let t = allocprofile::start();
        let t_m = mapprobe::start();
        let mut consist = Consistency::new_object_tables();
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), page.size());
        mapprobe::record(&mapprobe::CONS_NEW_NS, t_m);
        let t_m = mapprobe::start();
        let mut fa = take_or_new_frame_allocator();
        mapprobe::record(&mapprobe::TAKE_FA_NS, t_m);
        let t_m = mapprobe::start();
        let need = if PRECHARGE_EXACT {
            self.mapper.tables_needed(&cursor)
        } else {
            cursor.max_number_new_tables(Self::top_level(), 0)
        };
        if need > 0 {
            fa.precharge(need, FrameAllocFlags::WAIT_OK);
        }
        mapprobe::record(&mapprobe::PRECHARGE_NS, t_m);
        let t_m = mapprobe::start();
        let mut phys = ContiguousProvider::new(
            page.start_address(),
            page.size(),
            MappingSettings::default_user(),
        );
        mapprobe::record(&mapprobe::PROV_NS, t_m);
        allocprofile::record(&allocprofile::MAP_PREP_NS, t);
        let t = allocprofile::start();
        let t_m = mapprobe::start();
        let r = self.mapper.map(cursor, &mut phys, &mut consist, &mut fa);
        mapprobe::record(&mapprobe::WALK_NS, t_m);
        allocprofile::record(&allocprofile::MAP_WALK_NS, t);
        let t = allocprofile::start();
        let t_m = mapprobe::start();
        self.run_consistency(consist);
        mapprobe::record(&mapprobe::CONSIST_NS, t_m);
        allocprofile::record(&allocprofile::MAP_CONSIST_NS, t);
        // Explicit, and timed: everything above sums to well under a microsecond while the call
        // as a whole measures 35 us after mapping churn, and this drop is the only thing left.
        let t = allocprofile::start();
        let t_m = mapprobe::start();
        drop(fa);
        mapprobe::record(&mapprobe::DROP_FA_NS, t_m);
        let t_m = mapprobe::start();
        drop(phys);
        mapprobe::record(&mapprobe::DROP_PHYS_NS, t_m);
        allocprofile::record(&allocprofile::MAP_DROP_NS, t);
        mapprobe::record(&mapprobe::BODY_NS, t_body);
        r
    }

    /// Install a contiguous run of 4 KiB frames in one descent of the page tables.
    ///
    /// [`Self::map_page`] costs a walk from the root, a frame-allocator precharge and an
    /// invalidation pass *per page*, and the pager delivers ~130 pages per completion; this pays
    /// each of those once for the run. `Table::map` skips entries that are already present, taking
    /// no reference on their frames, so a run can be mapped whole without first being split around
    /// the pages the object turns out to hold.
    pub fn map_pages(
        &mut self,
        offset: u64,
        start: PhysAddr,
        npages: usize,
    ) -> Result<(), TwzError> {
        if npages == 0 {
            return Ok(());
        }
        let len = npages * PageNumber::PAGE_SIZE;
        let mut consist = Consistency::new_object_tables();
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), len);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Self::top_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        // Page-sized offers, not the whole run: these are separate frames, and a huge entry over
        // them would hold one refcount over memory owned by 512 of them. See
        // [`ContiguousProvider::new_of_page_size`].
        let mut phys = ContiguousProvider::new_of_page_size(
            start,
            len,
            PageNumber::PAGE_SIZE,
            MappingSettings::default_user(),
        );
        let r = self.mapper.map(cursor, &mut phys, &mut consist, &mut fa);
        self.run_consistency(consist);
        r
    }

    /// Which pages of a run the object already holds, and whether to a large entry.
    ///
    /// One descent, where asking per page was two of them each -- `is_empty_at_level` and then a
    /// `readmap` to tell a 4 KiB entry from the large page covering it. The reader yields only
    /// present entries, so what it does not report is what [`Self::map_pages`] will install.
    fn tally_present(&mut self, offset: u64, npages: usize) -> InstallTally {
        let len = npages * PageNumber::PAGE_SIZE;
        let end = offset + len as u64;
        let mut tally = InstallTally::default();
        for info in self.readmap(offset, len) {
            // A large entry reports its own aligned base and length, either of which can reach
            // outside the run, so count only the overlap.
            let lo = info.vaddr().raw().max(offset);
            let hi = (info.vaddr().raw().saturating_add(info.len() as u64)).min(end);
            let pages = hi.saturating_sub(lo) as usize / PageNumber::PAGE_SIZE;
            tally.dup += pages;
            if info.len() > PageNumber::PAGE_SIZE {
                tally.dup_large += pages;
            }
        }
        tally.dup = tally.dup.min(npages);
        tally.dup_large = tally.dup_large.min(tally.dup);
        tally.installed = npages - tally.dup;
        tally
    }

    pub fn readmap(&'_ mut self, offset: u64, len: usize) -> MapReader<'_> {
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), len);
        self.mapper.readmap(cursor)
    }

    pub fn with_mapper<R>(&mut self, f: impl FnOnce(&mut Mapper) -> R) -> R {
        f(&mut self.mapper)
    }

    pub fn print_tree(&self) {
        self.mapper.print_tables();
    }

    pub fn count_pages(&self) -> usize {
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), self.max_len());
        let reader = self.mapper.readmap(cursor).coalesce();
        reader.fold(0, |acc, mi| {
            if mi.is_empty() {
                acc
            } else {
                acc + mi.len() / PageNumber::PAGE_SIZE
            }
        })
    }

    /// Bucket every populated 2 MiB region of this object into `out`.
    ///
    /// One pass of the raw (uncoalesced) map reader: entries arrive in address order and never
    /// straddle a region, so grouping is a comparison against the running region base.
    pub fn promotion_census(&self, out: &mut PromotionCensus) {
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), self.max_len());
        let mut acc: Option<RegionAcc> = None;
        for mi in self.mapper.readmap(cursor) {
            if mi.is_empty() {
                continue;
            }
            let base = mi.vaddr().raw() & !(PHYS_LEVEL_LAYOUTS[1].size() as u64 - 1);
            if acc.as_ref().is_some_and(|acc| acc.base != base) {
                acc.take().unwrap().record(out);
            }
            acc.get_or_insert_with(|| RegionAcc::new(base)).add(&mi);
        }
        if let Some(acc) = acc {
            acc.record(out);
            out.objects += 1;
        }
    }

    pub fn get_dirty_and_reset(&mut self) -> Result<DirtyList, TwzError> {
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), self.max_len());

        fn add_to_list(dirty_list: &mut DirtyList, mi: &MapInfo) {
            fn can_append(mi: &MapInfo, item: &(PageNumber, PhysAddr, usize)) -> bool {
                if mi.is_empty() {
                    return false;
                }
                let pn = PageNumber::from_address(mi.vaddr());
                item.0.offset(item.2) == pn
                    && item
                        .1
                        .offset(item.2 * PageNumber::PAGE_SIZE)
                        .is_ok_and(|x| x == mi.paddr())
            }

            let frame = get_frame(mi.paddr()).expect("frame should exist");
            assert!(frame.size() == mi.len());
            dirty_list.frames.push(frame);
            frame.inc_refcount();

            if let Some(pos) = dirty_list
                .pages
                .iter()
                .position(|item| can_append(mi, item))
            {
                dirty_list.pages[pos].2 += mi.len() / PageNumber::PAGE_SIZE;
            } else {
                dirty_list.pages.push((
                    PageNumber::from_address(mi.vaddr()),
                    mi.paddr(),
                    mi.len() / PageNumber::PAGE_SIZE,
                ));
            }
        }

        let mut consist = Consistency::new_object_tables();
        let mut dirty_list = DirtyList::default();
        let r = self.mapper.with_dirty_bits(
            cursor,
            |mi| {
                add_to_list(&mut dirty_list, &mi);
                true
            },
            &mut consist,
        );

        dirty_list.pages.sort_unstable_by_key(|x| x.0);

        self.run_consistency(consist);

        r?;
        Ok(dirty_list)
    }

    pub fn maybe_cow_at(&mut self, offset: u64, mark_dirty: bool) -> Result<bool, TwzError> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Self::top_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );

        let mut consist = Consistency::new_object_tables();
        let did_cow = self
            .mapper
            .cow_at(cursor, &mut consist, mark_dirty, &mut fa);

        self.run_consistency(consist);

        did_cow
    }

    pub fn with_frame<R>(
        &mut self,
        offset: u64,
        flags: FindFrameFlags,
        did_cow: &mut bool,
        f: impl FnOnce(usize, Option<FrameRef>) -> R,
    ) -> Result<R, TwzError> {
        *did_cow = false;
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        if flags.contains(FindFrameFlags::WRITE) {
            *did_cow = self.maybe_cow_at(offset, true)?;
        }
        let mut reader = self.mapper.readmap(cursor);
        let mut page_aligned_offset = offset & !(PHYS_LEVEL_LAYOUTS[0].size() as u64 - 1);
        let mut map_info = reader.next();
        if let Some(mi) = &map_info {
            page_aligned_offset = offset & !(mi.len() as u64 - 1);
        }
        if map_info
            .as_ref()
            .is_some_and(|mi| mi.vaddr().raw() != offset & !(mi.len() as u64 - 1))
        {
            map_info = None;
        }
        let frame_offset = map_info
            .as_ref()
            .map_or(page_aligned_offset as usize, |mi| mi.vaddr().raw() as usize);
        Ok(f(
            frame_offset,
            map_info.and_then(|mi| get_frame(mi.paddr())),
        ))
    }

    /// Get the frame at a given offset. Does not mark the frame dirty.
    pub fn get_frame(&mut self, offset: u64) -> Option<FrameRef> {
        let map_info = self.get_mapinfo(offset)?;
        get_frame(map_info.paddr())
    }

    pub fn get_mapinfo(&mut self, offset: u64) -> Option<MapInfo> {
        let cursor =
            MappingCursor::new(VirtAddr::new(offset).unwrap(), PHYS_LEVEL_LAYOUTS[0].size());
        let mut reader = self.mapper.readmap(cursor);
        reader
            .next()
            .filter(|x| x.vaddr().raw() == offset & !(x.len() as u64 - 1))
    }

    pub fn is_empty_at_level(&mut self, offset: u64, level: usize) -> bool {
        let cursor = MappingCursor::new(
            VirtAddr::new(offset).unwrap(),
            PHYS_LEVEL_LAYOUTS[level].size(),
        );
        self.mapper.is_empty_at_level(&cursor, level)
    }

    pub fn split_to_level(&mut self, offset: u64, level: usize) -> Result<(), TwzError> {
        let mut consist = Consistency::new_object_tables();
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(Self::top_level(), FrameAllocFlags::WAIT_OK);
        let r = self.mapper.split_to_level(
            VirtAddr::new(offset).unwrap(),
            level,
            &mut consist,
            &mut fa,
        );
        self.run_consistency(consist);
        r
    }

    pub fn setup_cow_range(
        &mut self,
        dest: &mut Self,
        src_offset: u64,
        dst_offset: u64,
        len: usize,
    ) -> Result<(), TwzError> {
        let src_cursor = MappingCursor::new(VirtAddr::new(src_offset).unwrap(), len);
        let dst_cursor = MappingCursor::new(VirtAddr::new(dst_offset).unwrap(), len);
        let mut consist = Consistency::new_object_tables();
        let total = src_cursor.max_number_new_tables(Self::top_level(), 0)
            + dst_cursor.max_number_new_tables(Self::top_level(), 0);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(total, FrameAllocFlags::WAIT_OK);
        self.mapper.setup_cow_range(
            &mut dest.mapper,
            src_cursor,
            dst_cursor,
            &mut consist,
            &mut fa,
        )?;
        self.run_consistency2(consist, dest);
        Ok(())
    }

    pub fn setup_zero_range(&mut self, offset: u64, len: usize) -> Result<(), TwzError> {
        let cursor = MappingCursor::new(VirtAddr::new(offset).unwrap(), len);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Self::top_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        let mut consist = Consistency::new_object_tables();
        let ops = self.mapper.setup_zero_range(cursor, &mut consist, &mut fa);
        self.run_consistency(consist);
        ops
    }
}

impl Object {
    pub fn map_phys(
        &self,
        offset: usize,
        start: PhysAddr,
        end: PhysAddr,
        ct: CacheType,
    ) -> Result<(), TwzError> {
        let mut pt = self.lock_page_tables();
        let len = (end.raw() - start.raw()) as usize;
        let cursor = MappingCursor::new(VirtAddr::new(offset as u64).unwrap(), len);
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(pt.mapper.start_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        let mut phys =
            ContiguousProvider::new(start, len, MappingSettings::default_user().with_cache(ct));
        let mut consist = Consistency::new_object_tables();
        let r = pt.mapper.map(cursor, &mut phys, &mut consist, &mut fa);
        pt.run_consistency(consist);
        r
    }

    pub fn add_frame(&self, pn: PageNumber, frame: FrameRef) {
        let mut pt = self.lock_page_tables();
        pt.map_page(pn.as_byte_offset() as u64, frame).unwrap();
    }

    /// Install `npages` contiguous frames starting at `start` at object page `pn`, skipping any
    /// page the object already holds. Reports how the run was disposed of.
    ///
    /// The pager can deliver a page the object acquired between the request being issued and the
    /// completion landing; two overlapping in-flight requests produce those by construction, since
    /// `add_request` coalesces only on an exact range. `Table::map` already declines to overwrite a
    /// present entry -- and takes no reference on its frame, so the caller's release still frees it
    /// -- which is what lets the whole run go down in one call rather than being split around them.
    ///
    /// Taken as a run rather than per page because everything here is charged per *call*, not per
    /// page: one lock acquisition, one presence pass, one walk from the root, one precharge, one
    /// invalidation. At ~130 pages a completion that is the difference between 130 TLB shootdown
    /// rounds and one.
    pub fn add_frames_if_absent(
        &self,
        pn: PageNumber,
        start: PhysAddr,
        npages: usize,
    ) -> InstallTally {
        let mut pt = self.lock_page_tables();
        let offset = pn.as_byte_offset() as u64;
        let tally = pt.tally_present(offset, npages);
        pt.map_pages(offset, start, npages).unwrap();
        tally
    }

    pub fn cow_clone_page_tables(self: &ObjectRef) -> Result<ObjectPageTable, TwzError> {
        let mut new_pt = ObjectPageTable::new();
        let mut old_pt = self.lock_page_tables();
        assert_eq!(old_pt.mapper.start_level(), new_pt.mapper.start_level());
        let cursor = MappingCursor::new(VirtAddr::new(0).unwrap(), old_pt.max_len());
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(old_pt.mapper.start_level(), 0),
            FrameAllocFlags::WAIT_OK,
        );
        let mut consist = Consistency::new_object_tables();
        if self.use_pager() {
            old_pt = self.ensure_in_core(
                old_pt,
                PageNumber::from(0),
                cursor.remaining() / PageNumber::PAGE_SIZE,
                &mut false,
                &mut false,
            )?;
        }
        let r = old_pt.mapper.setup_cow_range(
            &mut new_pt.mapper,
            cursor,
            cursor,
            &mut consist,
            &mut fa,
        );
        old_pt.run_consistency(consist);
        r.map(|_| new_pt)
    }
}

/// What large-page *promotion* -- merging a fully-populated 2 MiB region of 4 KiB frames in place
/// -- would find across the object system.
///
/// A large page today is a property of delivery, not of state: it exists only where 512 aligned,
/// contiguous pages arrive in a single pager completion, and a region filled 4 KiB at a time stays
/// 4 KiB forever however contiguous it turns out to be (`largepager.md`). `promotable` is what a
/// promotion pass would convert and is the number that decides whether promotion is worth building.
/// `unaligned` is what it could not convert, and so sizes the pager-side object-keyed allocation
/// that would make promotion always possible.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct PromotionCensus {
    /// Objects with at least one populated region.
    pub objects: usize,
    /// Regions already mapped as one large page.
    pub large: usize,
    /// Full at 4 KiB, physically contiguous, 2 MiB-aligned, and made of frames a merge could
    /// actually take: singly-referenced, not COW, not wired, not page tables.
    pub promotable: usize,
    /// Contiguous and aligned, but the frames are shared -- refcount above one, or COW, or wired.
    /// `merge_frame`'s callers assert against exactly these, and a COW clone of an already-large
    /// region lands here, so counting it as the prize would inflate it by the number of clones.
    pub shared: usize,
    /// Full at 4 KiB, but fragmented or misaligned.
    pub unaligned: usize,
    /// Populated but not full, and the pages in them.
    pub partial: usize,
    pub partial_pages: usize,
    /// Populated region 0s, counted apart: page 0 is the null page and is never mapped, so region
    /// 0 can never be full and would only inflate `partial`.
    pub region0: usize,
    /// Pages in `region0` and `partial` regions -- the two buckets whose page count is not implied
    /// by their region count.
    pub loose_pages: usize,
}

impl PromotionCensus {
    /// Every 4 KiB page the census saw. The region counts claim memory, and the machine has to
    /// actually have it -- comparing this against the allocator is what makes them checkable.
    pub fn pages(&self) -> usize {
        let per_region = PHYS_LEVEL_LAYOUTS[1].size() / PageNumber::PAGE_SIZE;
        (self.large + self.promotable + self.shared + self.unaligned) * per_region
            + self.loose_pages
    }
}

/// One region's worth of accumulation for [ObjectPageTable::promotion_census].
struct RegionAcc {
    base: u64,
    bytes: usize,
    large: bool,
    /// The physical address this region would start at, as implied by an entry's offset within it.
    /// One value agreed by every entry is exactly what "physically contiguous" means here.
    phys_base: Option<u64>,
    contig: bool,
    /// Every frame is one a merge could take. Checked only while `contig` still holds, since a
    /// region that has already lost contiguity cannot be promoted whatever its frames look like --
    /// which keeps the frame lookups off the common fragmented case.
    ///
    /// Mapping settings are not compared: object page tables map with `default_user()` throughout,
    /// and the one thing that varies them is COW, which this already rejects.
    frames_ok: bool,
}

impl RegionAcc {
    fn new(base: u64) -> Self {
        Self {
            base,
            bytes: 0,
            large: false,
            phys_base: None,
            contig: true,
            frames_ok: true,
        }
    }

    fn add(&mut self, mi: &MapInfo) {
        self.bytes += mi.len();
        if mi.len() >= PHYS_LEVEL_LAYOUTS[1].size() {
            self.large = true;
        }
        let implied = mi.paddr().raw().wrapping_sub(mi.vaddr().raw() - self.base);
        match self.phys_base {
            None => self.phys_base = Some(implied),
            Some(phys_base) if phys_base != implied => self.contig = false,
            _ => {}
        }
        if self.contig && self.frames_ok {
            self.frames_ok = get_frame(mi.paddr()).is_some_and(|frame| {
                frame.refcount() == 1 && !frame.is_cow() && !frame.is_wired() && !frame.is_pt()
            });
        }
    }

    fn record(self, out: &mut PromotionCensus) {
        let region = PHYS_LEVEL_LAYOUTS[1].size();
        if self.base == 0 {
            out.region0 += 1;
            out.loose_pages += self.bytes / PageNumber::PAGE_SIZE;
        } else if self.large {
            out.large += 1;
        } else if self.bytes == region {
            let aligned = self
                .phys_base
                .is_some_and(|phys_base| phys_base.is_multiple_of(region as u64));
            if self.contig && aligned {
                if self.frames_ok {
                    out.promotable += 1;
                } else {
                    out.shared += 1;
                }
            } else {
                out.unaligned += 1;
            }
        } else {
            out.partial += 1;
            out.partial_pages += self.bytes / PageNumber::PAGE_SIZE;
            out.loose_pages += self.bytes / PageNumber::PAGE_SIZE;
        }
    }
}
