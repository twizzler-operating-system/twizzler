use alloc::vec::Vec;
use core::{
    alloc::Layout,
    sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
};

use bitflags::bitflags;
use intrusive_collections::{LinkedList, intrusive_adapter};
use twizzler_abi::{pager::PhysRange, thread::ExecutionState};

use super::{
    PhysAddr,
    frame::{
        FrameRef, PHYS_LEVEL_LAYOUTS, PhysicalFrameFlags, check_overlap, get_frame, split_frame,
    },
    framecache,
};
use crate::{
    arch::memory::frame::FRAME_SIZE,
    condvar::CondVar,
    once::{Once, OnceWait},
    processor::{
        sched::{SchedFlags, schedule},
        tls_ready,
    },
    spinlock::Spinlock,
    syscall::sync::{add_all_to_requeue, finish_blocking, requeue_all},
    thread::{Thread, ThreadRef, current_thread_ref, entry::start_new_kernel, priority::Priority},
};

/// Counters for the frame-allocation path, differenced by [`crate::perfmark`].
///
/// The question they exist to answer: after a workload has churned mappings, a zero-fill fault
/// costs 20x what it did fresh, and the cost is inside `ensure_in_core`'s frame acquisition
/// (`sysbench.md` F4). These separate the candidate explanations -- inline zeroing because the
/// zeroed pool ran dry, waiting for memory, and the reclaim thread being signalled (and spinning)
/// on every allocation once `should_reclaim` latches true, which it does permanently because
/// `reclaim_main` frees nothing.
pub mod allocprofile {
    use core::sync::atomic::{AtomicU64, Ordering};

    /// Timing spans, unlike the counts, cost two clock reads per frame allocation.
    pub const TIME_ALLOCS: bool = false;

    /// Whether `precharge` fills itself through [`crate::memory::tracker::try_alloc_frames`], one
    /// allocator-lock acquisition per batch, instead of one per frame.
    ///
    /// The measurement that motivates it is sound -- a frame allocation costs 1.3-2.8 us of which
    /// only ~300-650 ns is the zeroing, so three quarters is the per-frame acquisition and
    /// free-list walk, and 23% of precharge calls fetch ~3.75 frames each.
    ///
    /// History: the first enablement (`fa-bulk` round 1) hit `attempted to insert an object that
    /// is already linked` in the scheduler run queue. The primary event in that log is a
    /// kernel-mode instruction fetch at rip 0 with rsp in the kernel heap -- a live, heap-backed
    /// kernel stack read back a zeroed return address -- so the working theory is a physical frame
    /// reaching two owners, one of them zeroing it. `check_overlap` in `frame.rs` now panics at
    /// hand-out/free if a frame's range overlaps another admitted frame, naming both. 18 bench
    /// rounds with this flag on and the detectors armed did not reproduce it (regionremodel.md
    /// "diagnosis attempt"), and neither did the wide sweep that gated this flag: tag `bulkwide`,
    /// 2026-08-19, 54 armed rounds at -j6, zero tripwire hits (3 failures, all pre-existing
    /// families with BULK-off precedent).
    ///
    /// On per Daniel 2026-08-19 for soak coverage. The measured A/B is null-to-negative
    /// (`bulknum-off`/`-on`, -j1: create flat, contended create +22%, map/unmap +9% -- the batch
    /// holds the allocator lock across up to 32 allocations, lengthening the convoy), so this
    /// buys exposure, not speed. The successor is [FA_FREE_TO_POOL], which amortizes across
    /// operations rather than within one -- and on the free side, where the per-frame lock
    /// acquisitions actually are.
    pub const BULK_PRECHARGE: bool = true;

    /// Free-side feeding of the existing per-cpu pool (`TLS_FRAME_ALLOCATOR`): `free_frame`
    /// parks a level-0 frame there instead of re-entering the PFA. The alloc side already
    /// batches its PFA traffic; the free side took the lock once per frame, ~866k times a boot,
    /// and that asymmetry is what this removes. Supersedes the separate per-cpu cache faplan.md
    /// originally proposed, which duplicated this pool at 1/16 its size and behind it.
    pub const FA_FREE_TO_POOL: bool = true;

    /// Alloc-side counterpart of [`FA_FREE_TO_POOL`]: the global entry points
    /// ([`super::MemoryTracker::try_alloc_frame`] / `try_alloc_frames`) draw from this cpu's pool
    /// before touching the `PhysicalFrameAllocator`.
    ///
    /// The asymmetry this removes is the one the free-side comment above mis-states. "The alloc
    /// side already batches its PFA traffic" is true only of `precharge`; the pool is *fed* by
    /// every `free_frame` in the kernel and *drained* only by the eleven
    /// `take_or_new_frame_allocator` sites, so on any path that builds its allocator with
    /// `FrameAllocator::new` -- the whole object-data path -- it is write-only. Measured in
    /// `many-base2`'s `object_create_delete_nomap` window: 6,803 of 1,637,592 frees parked
    /// (0.42%), 845,041 declined `full`, while 818,402 of 1,629,013 frame allocations were
    /// *singular* trips to the global PFA that never consulted the pool at all.
    ///
    /// Draining here closes the cycle: the pool empties, parking stops declining `full`, and the
    /// frames recycle on the cpu that freed them.
    ///
    /// Known cost, and it is why this is a measured arm rather than an obvious win: a parked
    /// frame is dirty and never returns to the PFA, so the *background zeroer*
    /// (`frame::background_zero_iter`) cannot reach it. On `nomap` the global path serves 88% of
    /// its frames pre-zeroed (`zeroed=181,196` of `alloc=1,544,354` in `alloct1`), while every
    /// pool hand-out pays a 4 KiB memset inline (`pool-zeroed` ~= `parked` in every window).
    /// So this trades ~660 ns of global-allocator round trip for ~367 ns of inline zeroing.
    pub const FA_ALLOC_FROM_POOL: bool = true;

    /// Stop `take_or_new_frame_allocator` moving the pool out of the TLS slot. Operations get a
    /// fresh, empty allocator and *steal* frames from the pool through [`FA_ALLOC_FROM_POOL`]'s
    /// drain as they need them, instead of borrowing the whole thing for their duration.
    ///
    /// **The defect this removes is measured.** With the pool borrowed exclusively, it is `None`
    /// to everything else on that cpu for the length of the operation. In `poolval-98`,
    /// `page_fault_zero_fill`'s drain misses were **412,764 `no-pool` against 2 `empty`** --
    /// 99.9995% of them were "somebody is holding it", not "it is out of frames". The holder is
    /// `map_page` itself: `MAP_PREP_NS` brackets `take_or_new_frame_allocator()`.
    ///
    /// Requires [`FA_ALLOC_FROM_POOL`]. Without the drain there is no other way into the pool, so
    /// this alone would make it purely write-only and is a straight regression.
    ///
    /// Two hazards it introduces, both handled here rather than left to be discovered:
    ///
    /// 1. **`trim`/`clear` would free into the pool they are draining.** They call `free_frame`,
    ///    which parks. Today that is harmless because `Drop` runs on a *taken* allocator, so the
    ///    slot is `None` and the park declines. With the slot always populated, `trim` pops a frame
    ///    and immediately pushes it back: bounded by `TRIM_PER_DROP`, so not a live loop, but a
    ///    no-op that still counts `FA_TRIMMED` -- and it is the *only* thing that returns pooled
    ///    memory under pressure. The `MemoryState::Loaded` band is where it bites: `trim` is active
    ///    there (target `MAX/4`) and parking is still permitted (it only stops at `Tight`). Fixed
    ///    by [`free_frame_nopark`].
    /// 2. **An unbounded pop loop with interrupts off.** A `precharge` for `max_number_new_tables`
    ///    over a whole object asks for ~1030 frames; stealing them one at a time inside one
    ///    `with_disabled` is a long interrupts-off region. Bounded by [`MAX_UNPARK_BATCH`]; the
    ///    caller's remaining need falls through to the global allocator exactly as it does today.
    pub const FA_NO_TAKE: bool = true;

    /// Give the per-cpu pool a low watermark, so it stops sitting at the level where parking is
    /// refused.
    ///
    /// `MAX_TLS_PRECHARGE` is currently **both the ceiling and the trim target** --
    /// `park_frame_in_pool` refuses at it, and `trim`'s `MemoryState::Plenty` arm targets it, so
    /// excess is always zero and nothing ever drains. Measured consequence on
    /// `page_fault_zero_fill` (`notake-b`, per fault): 2.00 frames stolen from the pool, 1.00
    /// allocated from the global PFA, 1.01 freed, **0.007 parked, 99.3% of frees refused
    /// `full`**. Net demand on that bench is *zero* -- one frame consumed, one freed -- so every
    /// PFA operation on it exists only because the freed frame had nowhere to go.
    ///
    /// It is a priority inversion: the capacity is held by *circulating* surplus (2 stolen and 2
    /// saved per fault, never consumed) while *genuine* frees, the only inflow that would make
    /// recycling work, are the one thing turned away.
    ///
    /// With hysteresis the pool oscillates between the watermarks instead: it absorbs frees, it
    /// serves steals, and it touches the PFA only when it crosses a bound. Draining to the low
    /// water mark is also the only **cross-cpu** rebalance available here -- frames go back to the
    /// PFA, which every cpu can reach, and the ownership rules forbid touching another cpu's pool.
    pub const FA_POOL_WATERMARK: bool = true;

    /// Over-fetch from the global allocator so its lock is amortized across faults.
    ///
    /// The pool already *returns* frames in bulk (the watermark drain); it still *acquires* them
    /// a couple at a time, so a cpu whose pool is empty pays a PFA acquisition on every fault.
    /// Measured on `page_fault_zero_fill`: **1.00 global allocation per fault in every arm so
    /// far**, with `pooled` concentrated on one cpu and 469,779 `empty` steal-misses on the
    /// others -- the frees and the faults are on different cpus, and no per-cpu scheme can move
    /// frames between them. The PFA is the only rebalancer this design permits.
    ///
    /// So when a precharge has to reach the global allocator anyway, take [`POOL_REFILL_BATCH`]
    /// frames under that one acquisition instead of the one or two asked for. The caller keeps
    /// what it needs and `Drop` hands the rest to the pool through the save that already exists,
    /// making the next `POOL_REFILL_BATCH - 1` faults on this cpu pool hits. That turns "one lock
    /// per fault" into "one lock per batch" by construction, whichever cpu does the freeing.
    ///
    /// Deliberately in `precharge`/`precharge_nowait` rather than `try_alloc_frames`: the two
    /// other callers of the batch path are `#[kernel_test]`s that assert on exactly what they
    /// asked for, and one of them is level-0.
    ///
    /// Gated on `MemoryState::Plenty` -- over-fetching reserves `idle` for frames nobody asked
    /// for, which is the wrong move when memory is short.
    pub const FA_POOL_BULK_REFILL: bool = true;

    /// A/B: keep the pool-sized eager reserve in `precharge` even on a per-operation allocator.
    ///
    /// `true` restores the behaviour the [`FA_NO_TAKE`] flip shipped with. `false` sizes a
    /// per-operation allocator's vec for what it will actually hold, which is the fix for the
    /// 16 KB heap allocation per `map_page` described at the reserve itself.
    ///
    /// **Measured, and the small arm lost** (`resfix`, isolated `page_fault_zero_fill`, against
    /// `knobs-on`). Sizing the per-operation vec at 98 slots instead of 2,080 did **not** make
    /// `precharge` cheaper -- 620 ns -> 729 ns -- so the cost is the heap allocation itself, not
    /// its size. And it did what this comment warned it might: the TLS pool inherits its vec from
    /// whichever operation `merge` swapped into an empty slot, so `FA_PARK_NO_CAP` went
    /// 0 -> 1,504,627, `parked` 1,639,371 -> 455 and `leftover` 0 -> 669,337. Parking stopped
    /// working entirely, and the bench went 2,873 -> 3,319 ns.
    ///
    /// Two things worth keeping from that: the pool's 2,048-frame depth exists only *as a side
    /// effect* of this reserve being pool-sized, which is a fragile way to size a per-cpu
    /// structure; and the real fix is for a per-operation allocator not to own heap storage at
    /// all, rather than to own less of it.
    ///
    /// Left `true` (the shipped behaviour) so the arm is recorded rather than re-derived.
    pub const FA_OP_RESERVE_POOL_SIZED: bool = true;

    /// Whether the unpark path runs `check_overlap`.
    ///
    /// Separate from the park-side check **because it cannot be shared setup**: it only executes
    /// on the pool path, so an arm with the drain off never pays it. Calling it "unconditional,
    /// rides in all arms" was wrong, and it is the leading suspect for `notake-b`'s +12.6% on
    /// `page_fault_soft_contended` (2.33 unparks per precharge call there). Off in timed arms,
    /// on in a separate armed boot.
    pub const FA_UNPARK_OVERLAP_CHECK: bool = false;

    macro_rules! counters {
        ($($name:ident),* $(,)?) => {
            $(pub static $name: AtomicU64 = AtomicU64::new(0);)*
            pub const NAMES: &[&str] = &[$(stringify!($name)),*];
            pub const NR: usize = NAMES.len();
            /// Snapshot in declaration order, to be differenced against a later one.
            pub fn snapshot() -> [u64; NR] {
                [$($name.load(Ordering::Relaxed)),*]
            }
        };
    }

    counters!(
        ALLOCS,
        ALLOC_NS,
        ZEROED_INLINE,
        ZERO_NS,
        WAITS,
        WAIT_NS,
        FREES,
        RECLAIM_SIGNALS,
        RECLAIM_WAKES,
        RECLAIM_ROUNDS,
        FILL_ITERS,
        FILL_LOOP_NS,
        FILL_EMPTY_NS,
        FILL_TAKE_NS,
        FILL_MAP_NS,
        FILL_MAP_LT1US,
        FILL_MAP_LT10US,
        FILL_MAP_LT100US,
        FILL_MAP_GE100US,
        FILL_MAP_INTS,
        MAP_PREP_NS,
        MAP_WALK_NS,
        MAP_CONSIST_NS,
        PROBE_NS,
        MAP_DROP_NS,
        FA_DROP_SAVED,
        FA_DROP_CLEARED,
        FA_DROP_SAVE_NS,
        FA_DROP_CLEAR_NS,
        FA_DROP_FRAMES,
        FA_TRIMMED,
        // Appended, not inserted: `perfmark` indexes this snapshot positionally.
        // Retired with the global lock: these three can no longer fire and read 0 for good.
        // Kept rather than deleted because `perfmark` indexes this snapshot positionally, and
        // removing them would shift every later index -- which is exactly the break made and
        // fixed earlier tonight. A future append can reuse the slots; a delete cannot.
        FA_TAKE_LOCKED,
        FA_TAKE_NONE,
        FA_SAVE_LOCKED,
        FA_ALLOC_POOL,
        FA_ALLOC_GLOBAL,
        FA_ALLOC_AVOID_EMPTY,
        // `precharge` calls served entirely from the pool, versus frames it had to fetch from the
        // global tracker. The `FA_ALLOC_*` counters above sit in `try_allocate`, which is
        // downstream of this -- the pool is a staging buffer that `precharge` fills immediately
        // before use, not a cache that avoids the global allocator.
        PRECHARGE_CALLS,
        PRECHARGE_EARLY,
        PRECHARGE_FETCHED,
        // Appended, as the warning above says. These were briefly inserted before `FA_DROP_SAVED`,
        // which shifted every later index by three and silently mislabelled the PERFMARK-DROP and
        // PERFMARK-FA lines in every boot in between.
        FA_PARKED,
        FA_PARK_LOCKED,
        FA_POOL_ZEROED,
        // Appended, as the warning above says.
        //
        // Park-decline attribution. Parking can be *on*, the pool can be full, and the free path
        // can still park nothing -- which is what `object_create_delete_nomap` does. Separating
        // "parking does not pay" from "parking never gets a turn" is what made the first A/B
        // unreadable; these say which decline it was.
        FA_PARK_NOT_L0,
        FA_PARK_NO_TLS,
        FA_PARK_PRESSURE,
        FA_PARK_NO_POOL,
        FA_PARK_FULL,
        FA_PARK_NO_CAP,
        // Save path. `merge` runs from `Drop`, and a vec growth there is an allocation in a
        // context that must not allocate (faplan.md's hazard note). `FA_SAVE_GREW` is the
        // hazard *firing*, whichever line caused it; `FA_SAVE_APPEND` is the branch reachability
        // question underneath it.
        FA_SAVE_APPEND,
        // Split deliberately: `append` growth is bounded by the pool, `extend` growth by
        // MAX_FA_FRAMES, and they want differently-sized reserves. One counter firing for both
        // would say the hazard is live without saying which fix sizes it.
        FA_SAVE_GREW_APPEND,
        FA_SAVE_GREW_EXTEND,
        // `ensure_pt_zeroed` exposure. `PT_DIRTY` alone reads identically whether the tripwire
        // examined a million frames or none, so the examined count travels with it.
        PT_CHECKED,
        PT_DIRTY,
        // Frames a bounded merge declined to take because the pool had no room. They are freed
        // by `Drop`'s `clear()` instead -- the cost of making the save path non-allocating.
        FA_MERGE_LEFTOVER,
        // The batch path's time and frame count. `ALLOC_NS` covers `try_alloc_frame` only; with
        // `BULK_PRECHARGE` on, most frames come from `try_alloc_frames` and contributed no time
        // at all -- so `alloc=N/Xus` read as almost-free for a reason that had nothing to do with
        // how fast allocation is.
        ALLOC_BULK_NS,
        ALLOC_BULK_FRAMES,
        // Appended, as the warning above says. The alloc-side drain: frames served from this
        // cpu's pool by the global entry points, and calls that found no frame there. `MISS` is
        // what separates "the drain is off" from "the drain runs and the pool is empty" -- the
        // same distinction the park declines had to be split to get.
        FA_UNPARKED,
        FA_UNPARK_MISS,
        // Appended. Splits `FA_UNPARK_MISS` into its two causes, which decide between the two
        // live explanations for the drain being inert on `nomap`: `NO_POOL` (the slot is `None`,
        // an operation holds the pool -- a *temporal* story) versus `EMPTY` (the slot is `Some`
        // and the vec is empty -- a per-cpu *distribution* story, frames parked on cpus that are
        // not allocating). `NO_POOL + EMPTY` must reconcile to `MISS`, or neither reading counts.
        FA_UNPARK_NO_POOL,
        FA_UNPARK_EMPTY,
        // Appended. **PFA lock acquisitions**, which is the quantity the goal is stated in and
        // which no existing counter measures: `ALLOCS` counts *frames*, so a batch of 64 taken
        // under one acquisition reports as 64. Acquisitions per fault is the number that says
        // whether the lock has actually been amortized away.
        ALLOC_BULK_CALLS,
        ALLOC_SINGLE_CALLS,
        // Appended, not inserted -- `perfmark` indexes this snapshot positionally.
        //
        // Times a cpu's pool buffer was installed. Should be at most one per cpu for a whole
        // boot; anything more means something is still handing the pool a smaller vec and the
        // provisioning is fighting it.
        FA_POOL_PROVISIONED,
        // Times a [`FrameStore`] outgrew its inline capacity and moved to the heap. Appended, so
        // the positional indices `perfmark` uses are unchanged. This is the counter that says
        // whether `FA_INLINE_CAP` is the right size: a per-operation allocator should never spill,
        // and the pool spills exactly once per cpu at provisioning.
        FA_SPILL,
    );

    /// Nanoseconds since `start`, for a caller that wants the number as well as the counter.
    pub fn elapsed_ns(start: crate::instant::Instant) -> u64 {
        if !TIME_ALLOCS {
            return 0;
        }
        let dur: twizzler_abi::syscall::TimeSpan = (crate::instant::Instant::now() - start).into();
        dur.as_nanos() as u64
    }

    /// Bucket one `map_page` by cost. A 35 us mean is either every call or a few enormous ones,
    /// and those have opposite explanations. Callers gate this on [`TIME_ALLOCS`]: with timing
    /// off every `ns` is zero and the histogram would read as uniformly fast.
    pub fn record_map_bucket(ns: u64) {
        add(
            if ns < 1_000 {
                &FILL_MAP_LT1US
            } else if ns < 10_000 {
                &FILL_MAP_LT10US
            } else if ns < 100_000 {
                &FILL_MAP_LT100US
            } else {
                &FILL_MAP_GE100US
            },
            1,
        );
    }

    pub fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }

    /// Read the clock only when the answer will be used.
    pub fn start() -> crate::instant::Instant {
        if TIME_ALLOCS {
            crate::instant::Instant::now()
        } else {
            crate::instant::Instant::zero()
        }
    }

    pub fn record(c: &AtomicU64, start: crate::instant::Instant) {
        if !TIME_ALLOCS {
            return;
        }
        let dur: twizzler_abi::syscall::TimeSpan = (crate::instant::Instant::now() - start).into();
        add(c, dur.as_nanos() as u64);
    }
}

pub struct MemoryTracker {
    kernel_used: AtomicUsize,
    page_data: AtomicUsize,
    idle: AtomicUsize,
    total: AtomicUsize,
    allocated: AtomicUsize,
    freed: AtomicUsize,
    reclaimed: AtomicUsize,
    waiting: AtomicUsize,
    pager_outstanding: AtomicUsize,
    /// `OnceWait`, not `Once`: `Once::poll` spins while the initializer is `RUNNING`, and the
    /// callers below reach it from places that cannot spin -- `trigger_reclaim` runs inside
    /// `MemoryTracker::wait`'s `enter_critical()` and on every `try_alloc_frame`, and the reclaim
    /// thread polls this while its own creator is still inside `call_once`. `OnceWait::poll`
    /// returns `None` instead of spinning, which is what those callers actually want.
    reclaim: OnceWait<ReclaimThread>,
    waiters: Spinlock<LinkedList<LinkAdapter>>,
}
intrusive_adapter!(pub LinkAdapter = ThreadRef: Thread { memwait_link: intrusive_collections::linked_list::AtomicLink });

/// Coarse, read-mostly memory-pressure level.
///
/// Replaces `is_low_mem()` on the paths that only want a policy hint. That predicate recomputes
/// `page_data >= idle/2 || idle < kernel*2` from three `Acquire` loads and a division at every
/// call; this is one `Relaxed` byte load off a line that is shared in every cpu's cache. More
/// importantly it can *recover*: `page_cond`'s first term cannot, because `page_data` does not
/// shrink, which is the latch `trigger_reclaim`'s comment measures at 361,690 spurious reclaim
/// wakes per 1.4M allocations -- currently masked by a workaround that the same comment says has
/// to go when reclaim's steps 1-5 land.
///
/// Advisory only. Anything whose correctness depends on an exact answer keeps its exact predicate.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum MemoryState {
    Plenty = 0,
    Loaded = 1,
    Tight = 2,
    Emergency = 3,
}

/// Idle-frame percentages of total at which the bands meet, most-free first.
const BAND_PCT: [usize; 3] = [25, 10, 3];
/// A band is left in the *recovering* direction only once idle is this many points past the
/// threshold it would cross, so a workload sitting on a boundary cannot flap the free path
/// between parking and not parking.
const BAND_HYST_PCT: usize = 3;

static MEMORY_STATE: AtomicU8 = AtomicU8::new(MemoryState::Plenty as u8);
/// The idle range over which the current band is stable, so the common case costs two relaxed
/// loads and two compares rather than a recompute. Deliberately initialized to the empty range:
/// `total` is not known until [`init`], so the first check must fall through and compute the
/// real bounds rather than believing a window that was never derived from anything.
/// Frames currently parked in per-cpu pools. A *gauge*, not a counter: it is what tells a
/// reader how much of `kernel_used`/`page_data` is pool occupancy rather than live use, which
/// is otherwise indistinguishable and lands in the same mod-512 residual that harnesses use for
/// the page-table term. Bounded by `MAX_TLS_PRECHARGE` per cpu.
static POOLED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static MEM_STATE_LO: AtomicUsize = AtomicUsize::new(0);
static MEM_STATE_HI: AtomicUsize = AtomicUsize::new(0);

#[inline]
pub fn memory_state() -> MemoryState {
    match MEMORY_STATE.load(Ordering::Relaxed) {
        0 => MemoryState::Plenty,
        1 => MemoryState::Loaded,
        2 => MemoryState::Tight,
        _ => MemoryState::Emergency,
    }
}

fn raw_band(idle: usize, total: usize) -> MemoryState {
    let pct = if total == 0 { 100 } else { idle * 100 / total };
    if pct >= BAND_PCT[0] {
        MemoryState::Plenty
    } else if pct >= BAND_PCT[1] {
        MemoryState::Loaded
    } else if pct >= BAND_PCT[2] {
        MemoryState::Tight
    } else {
        MemoryState::Emergency
    }
}

/// Idle range `[lo, hi)` over which `state` stays put. The upper edge carries the hysteresis:
/// leaving a pressured band needs `BAND_HYST_PCT` more than entering it did.
fn band_bounds(state: MemoryState, total: usize) -> (usize, usize) {
    let f = |pct: usize| total * pct / 100;
    let hyst = |pct: usize| total * (pct + BAND_HYST_PCT) / 100;
    match state {
        MemoryState::Plenty => (f(BAND_PCT[0]), usize::MAX),
        MemoryState::Loaded => (f(BAND_PCT[1]), hyst(BAND_PCT[0])),
        MemoryState::Tight => (f(BAND_PCT[2]), hyst(BAND_PCT[1])),
        MemoryState::Emergency => (0, hyst(BAND_PCT[2])),
    }
}

impl MemoryTracker {
    /// Cheap check on the paths where idle moves. Recomputes only when idle leaves the current
    /// band's stable range.
    #[inline]
    fn note_idle_change(&self) {
        let idle = self.idle.load(Ordering::Relaxed);
        if idle >= MEM_STATE_LO.load(Ordering::Relaxed)
            && idle < MEM_STATE_HI.load(Ordering::Relaxed)
        {
            return;
        }
        self.recompute_memory_state(idle);
    }

    #[cold]
    fn recompute_memory_state(&self, idle: usize) {
        let total = self.total();
        let cur = memory_state();
        let raw = raw_band(idle, total);
        // Recovering (less pressure): only if past the crossed threshold plus hysteresis.
        let next = if raw < cur {
            let (_, hi) = band_bounds(cur, total);
            if idle >= hi { raw } else { cur }
        } else {
            raw
        };
        let (lo, hi) = band_bounds(next, total);
        MEM_STATE_LO.store(lo, Ordering::Relaxed);
        MEM_STATE_HI.store(hi, Ordering::Relaxed);
        if next != cur {
            MEMORY_STATE.store(next as u8, Ordering::Relaxed);
            // A store and nothing else: this runs inside `free_frame_inner`, and trimming here
            // would free frames straight back into it.
            framecache::request_trim(next);
            log::debug!(
                "memory state: {:?} -> {:?} ({} idle of {})",
                cur,
                next,
                idle,
                total
            );
        }
    }

    fn free_frame(&self, frame: FrameRef) {
        self.free_frame_inner(frame, true)
    }

    /// `allow_park = false` is for callers that are *draining* the pool: `FrameAllocator::trim`
    /// and `clear`. With [`allocprofile::FA_NO_TAKE`] on, the TLS slot is populated during
    /// `Drop`, so a parking free would push the frame straight back into the pool the caller is
    /// emptying -- bounded by `TRIM_PER_DROP`, so not a live loop, but a no-op that still counts
    /// `FA_TRIMMED` and returns no memory. See [`allocprofile::FA_NO_TAKE`] hazard 1.
    fn free_frame_inner(&self, frame: FrameRef, allow_park: bool) {
        allocprofile::add(&allocprofile::FREES, 1);
        let count = frame.size() / FRAME_SIZE;
        // Park before any accounting: a parked frame stays ALLOCATED and stays charged to its
        // class, which is exactly how every other frame in this pool is already treated, so the
        // free's counter writes are *skipped* rather than reproduced. No `wake()` either --
        // nothing became available to a waiter, which is consistent with parking stopping once
        // `MemoryState` reaches `Tight`, the only state in which waiters exist.
        if count == 1 && allow_park {
            if cache_freed_frame(frame) {
                return;
            }
            if park_frame_in_pool(frame) {
                allocprofile::add(&allocprofile::FA_PARKED, 1);
                return;
            }
        } else if count != 1 {
            allocprofile::add(&allocprofile::FA_PARK_NOT_L0, 1);
        }
        let old = if frame.is_kernel() {
            self.kernel_used.fetch_sub(count, Ordering::SeqCst)
        } else {
            self.page_data.fetch_sub(count, Ordering::SeqCst)
        };
        assert!(old > 0);
        self.idle.fetch_add(count, Ordering::SeqCst);
        self.freed.fetch_add(count, Ordering::SeqCst);
        crate::memory::frame::raw_free_frame(frame);
        self.note_idle_change();
        self.wake();
    }

    fn try_alloc_frame(&self, flags: FrameAllocFlags, layout: Layout) -> Option<FrameRef> {
        // This cpu's pool first. A parked frame is already `ALLOCATED`, already charged to a
        // class and never returned to `idle`, so serving one here skips `consider_reclaim`, the
        // `idle` CAS, the class and `allocated` counters, `note_idle_change` and the PFA lock --
        // which is 660 of the 703 ns a singular frame costs (`alloct1`; the remaining ~43 ns is
        // the 11.7%-weighted inline zeroing). `ALLOCS`/`ALLOC_NS` deliberately do not count it:
        // they mean "went to the global allocator", and every ratio in faplan.md reads them
        // that way.
        if layout == PHYS_LEVEL_LAYOUTS[0] {
            if let Some((frame, needs_zeroing)) = framecache::alloc_one(want_of(flags)) {
                return Some(finish_cached_alloc(frame, flags, needs_zeroing));
            }
        }
        // `!ENABLED` so exactly one cache is live in either arm. With both on, the old pool sits
        // behind the new one collecting nothing and costing an interrupts-off TLS read per miss --
        // and an A/B whose arms differ by "which cache" is readable in a way that one whose arms
        // differ by "one cache or two" is not.
        if !framecache::ENABLED
            && allocprofile::FA_ALLOC_FROM_POOL
            && layout == PHYS_LEVEL_LAYOUTS[0]
        {
            if let Some(frame) = unpark_frame_from_pool() {
                return Some(finish_parked_alloc(frame, flags));
            }
        }
        let t_alloc = allocprofile::start();
        let r = self.do_try_alloc_frame(flags, layout);
        allocprofile::add(&allocprofile::ALLOCS, 1);
        allocprofile::record(&allocprofile::ALLOC_NS, t_alloc);
        r
    }

    /// Allocate up to `want` frames in one pass, appending them to `out` and returning how many.
    ///
    /// The per-frame path takes the allocator lock, runs `consider_reclaim` and does a CAS on
    /// `idle` for every single frame. This does each once for the batch, which is what the
    /// measured cost is made of: 1.3-2.8 us per frame of which only ~300-650 ns is the zeroing
    /// that still happens per frame.
    ///
    /// Best-effort, like the singular version: a short return means memory ran out, and the caller
    /// decides whether to wait. Never waits itself.
    fn try_alloc_frames(
        &self,
        flags: FrameAllocFlags,
        layout: Layout,
        want: usize,
        out: &mut FrameStore,
    ) -> usize {
        if want == 0 {
            return 0;
        }
        // Same rationale as the singular path above. Bounded by `out`'s spare capacity because
        // this must not allocate: growing the vec reaches the kernel heap, and one of this
        // function's callers is `GlobalPageAlloc::extend` running under `GLOBAL_PAGE_ALLOC`,
        // which is the self-deadlock faplan.md hit deterministically.
        let mut from_pool = 0;
        if framecache::ENABLED && layout == PHYS_LEVEL_LAYOUTS[0] {
            // Collected inside the interrupts-off region and finished outside it: finishing can
            // memset 4 KiB. The closure refuses once `out` is at capacity rather than letting it
            // grow -- `GlobalPageAlloc::extend` is one of this function's callers and reaches here
            // holding `GLOBAL_PAGE_ALLOC`, where an allocation self-deadlocks.
            let start = out.len();
            // One bit per frame. Sound only because `alloc_many` caps itself at
            // `framecache::MAX_BATCH`, which is the width of this word -- see the const, and note
            // that the alternative here silently drops the flags past the 64th, which hands a
            // dirty frame to page-table code as zeroed.
            const _: () = assert!(framecache::MAX_BATCH <= u64::BITS as usize);
            let mut zeroing: u64 = 0;
            let got = framecache::alloc_many(want_of(flags), want, |frame, needs_zeroing| {
                // Refuse rather than grow: `GlobalPageAlloc::extend` reaches here holding
                // `GLOBAL_PAGE_ALLOC`, where an allocation self-deadlocks. `alloc_many` puts a
                // refused frame back in the cache.
                if out.len() == out.capacity() {
                    return false;
                }
                if needs_zeroing {
                    zeroing |= 1 << (out.len() - start);
                }
                out.push(frame);
                true
            });
            // Outside the interrupts-off region: finishing can memset 4 KiB.
            for i in 0..got {
                out[start + i] =
                    finish_cached_alloc(out[start + i], flags, zeroing & (1 << i) != 0);
            }
            from_pool += got;
            if from_pool >= want {
                return from_pool;
            }
        }
        if !framecache::ENABLED
            && allocprofile::FA_ALLOC_FROM_POOL
            && layout == PHYS_LEVEL_LAYOUTS[0]
        {
            let t_pool = crate::obj::pagetables::mapprobe::start();
            let start = out.len();
            from_pool = unpark_frames_from_pool(want, out);
            // Outside the interrupts-off region: `finish_parked_alloc` can memset 4 KiB.
            for i in start..out.len() {
                out[i] = finish_parked_alloc(out[i], flags);
            }
            crate::obj::pagetables::mapprobe::record(
                &crate::obj::pagetables::mapprobe::PC_POOL_NS,
                t_pool,
            );
            if from_pool >= want {
                return from_pool;
            }
        }
        let want = want - from_pool;
        // **After** the pool draw and bounded by what the pool can absorb -- both learned the
        // expensive way. Inflating before the draw made every precharge pull a whole batch,
        // use one frame and hand 63 back; unbounded, the surplus overflowed `merge` and left
        // through `clear()` **one frame at a time** (`leftover=2,274,529`, 15 global allocations
        // per fault, the bench 6.7x slower). Bulk in, singular out is worse than no bulk at all.
        //
        // Here it only enlarges a fetch that was already going to the global allocator, and only
        // by as much as `merge` will accept, so the surplus lands in the pool instead of
        // bouncing off it.
        let want = if allocprofile::FA_POOL_BULK_REFILL
            && layout == PHYS_LEVEL_LAYOUTS[0]
            && memory_state() == MemoryState::Plenty
        {
            // Bounded by whichever cache will actually absorb the surplus. With the frame cache on
            // the old pool is never provisioned, so `pool_headroom` reads 0 and this over-fetch
            // would go silently inert -- leaving the cache fed only by frees, which is exactly the
            // write-only failure the old pool spent three sessions in.
            let headroom = if framecache::ENABLED {
                framecache::headroom()
            } else {
                pool_headroom()
            };
            want.max(POOL_REFILL_BATCH.min(headroom))
        } else {
            want
        };
        let t_global = crate::obj::pagetables::mapprobe::start();
        crate::obj::pagetables::mapprobe::tick(&crate::obj::pagetables::mapprobe::PC_GLOBAL_CALLS);
        let pff = if flags.contains(FrameAllocFlags::ZEROED) {
            PhysicalFrameFlags::ZEROED
        } else {
            PhysicalFrameFlags::empty()
        };
        let per = layout.size() / FRAME_SIZE;
        let t_rec = crate::obj::pagetables::mapprobe::start();
        self.consider_reclaim();
        crate::obj::pagetables::mapprobe::record(
            &crate::obj::pagetables::mapprobe::G_RECLAIM_NS,
            t_rec,
        );
        let t_cas = crate::obj::pagetables::mapprobe::start();

        // Reserve the whole batch against `idle` in one CAS. Reserving what is there rather than
        // failing outright keeps this a best-effort call: the caller asked for `want` and takes
        // what it gets.
        let reserved = loop {
            let idle = self.idle();
            let can = (idle / per).min(want);
            if can == 0 {
                crate::obj::pagetables::mapprobe::record(
                    &crate::obj::pagetables::mapprobe::PC_GLOBAL_NS,
                    t_global,
                );
                return from_pool;
            }
            if self
                .idle
                .compare_exchange(idle, idle - can * per, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break can;
            }
        };

        crate::obj::pagetables::mapprobe::record(
            &crate::obj::pagetables::mapprobe::G_CAS_NS,
            t_cas,
        );
        out.reserve(reserved);
        let before = out.len();
        let t_raw = crate::obj::pagetables::mapprobe::start();
        let t_bulk = allocprofile::start();
        allocprofile::add(&allocprofile::ALLOC_BULK_CALLS, 1);
        let got = crate::memory::frame::raw_alloc_frames(pff, layout, reserved, out);
        allocprofile::record(&allocprofile::ALLOC_BULK_NS, t_bulk);
        crate::obj::pagetables::mapprobe::record(
            &crate::obj::pagetables::mapprobe::G_RAW_NS,
            t_raw,
        );
        allocprofile::add(&allocprofile::ALLOC_BULK_FRAMES, got as u64);
        allocprofile::add(&allocprofile::ALLOCS, got as u64);

        // Hand back what was reserved and not taken, or `idle` leaks by the difference.
        if got < reserved {
            self.idle
                .fetch_add((reserved - got) * per, Ordering::SeqCst);
        }
        if got == 0 {
            crate::obj::pagetables::mapprobe::record(
                &crate::obj::pagetables::mapprobe::PC_GLOBAL_NS,
                t_global,
            );
            return from_pool;
        }
        for frame in &out.as_slice()[before..] {
            assert!(
                frame.refcount() == 0,
                "allocated frame with non-zero refcount: {:?} {}",
                frame,
                frame.refcount()
            );
            frame.set_kernel(flags.contains(FrameAllocFlags::KERNEL));
        }
        let pages = got * per;
        if flags.contains(FrameAllocFlags::KERNEL) {
            self.kernel_used.fetch_add(pages, Ordering::SeqCst);
        } else {
            self.page_data.fetch_add(pages, Ordering::SeqCst);
        }
        self.allocated.fetch_add(pages, Ordering::SeqCst);
        crate::obj::pagetables::mapprobe::record(
            &crate::obj::pagetables::mapprobe::PC_GLOBAL_NS,
            t_global,
        );
        got + from_pool
    }

    fn do_try_alloc_frame(&self, flags: FrameAllocFlags, layout: Layout) -> Option<FrameRef> {
        let pff = if flags.contains(FrameAllocFlags::ZEROED) {
            PhysicalFrameFlags::ZEROED
        } else {
            PhysicalFrameFlags::empty()
        };
        loop {
            self.consider_reclaim();
            let idle = self.idle();

            let count = layout.size() / FRAME_SIZE;
            if idle >= count {
                let did_sub = self
                    .idle
                    .compare_exchange(idle, idle - count, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok();
                if did_sub {
                    allocprofile::add(&allocprofile::ALLOC_SINGLE_CALLS, 1);
                    if let Some(frame) = crate::memory::frame::raw_alloc_frame(pff, layout) {
                        assert!(
                            frame.refcount() == 0,
                            "allocated frame with non-zero refcount: {:?} {}",
                            frame,
                            frame.refcount()
                        );
                        if flags.contains(FrameAllocFlags::KERNEL) {
                            frame.set_kernel(true);
                            self.kernel_used.fetch_add(count, Ordering::SeqCst);
                        } else {
                            frame.set_kernel(false);
                            self.page_data.fetch_add(count, Ordering::SeqCst);
                        }
                        self.allocated.fetch_add(count, Ordering::SeqCst);
                        self.note_idle_change();
                        return Some(frame);
                    } else {
                        self.idle.fetch_add(count, Ordering::SeqCst);
                    }
                } else {
                    continue;
                }
            }

            if flags.contains(FrameAllocFlags::WAIT_OK) {
                let t_wait = allocprofile::start();
                self.wait(idle);
                allocprofile::add(&allocprofile::WAITS, 1);
                allocprofile::record(&allocprofile::WAIT_NS, t_wait);
            } else {
                return None;
            }
        }
    }

    fn try_alloc_split_frames(
        &self,
        flags: FrameAllocFlags,
        layout: Layout,
    ) -> Option<(FrameRef, usize)> {
        self.try_alloc_frame(flags, layout).map(|frame| {
            if frame.size() == PHYS_LEVEL_LAYOUTS[0].size() {
                (frame, frame.size())
            } else {
                split_frame(frame)
            }
        })
    }

    fn alloc_frame(&self, flags: FrameAllocFlags) -> FrameRef {
        self.try_alloc_frame(flags, PHYS_LEVEL_LAYOUTS[0])
            .expect("cannot wait for page")
    }

    fn wait(&self, old_idle: usize) {
        logln!(
            "thread waiting for memory alloc {} {}",
            old_idle,
            self.idle()
        );
        print_tracker_stats();
        let Some(current_thread) = current_thread_ref() else {
            panic!("warning -- cannot wait on memory before threading initialized");
        };
        crate::thread::locktrack::warn_if_blocking_with_mutexes("memory alloc");
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let guard = current_thread.enter_critical();
        self.waiters.lock().push_back(current_thread.clone());
        current_thread.set_sync_sleep_done();
        self.trigger_reclaim();
        {
            current_thread.set_state(ExecutionState::Sleeping);
            // Two reasons not to block after having registered. Memory may have become available
            // under us -- and a force-exit may have landed, which a thread parked here would never
            // see: `wake()` only fires when memory is freed, and the exit request is not that.
            // Unlike the pager sites, this one cannot park its own wakeup and block anyway: being
            // woken through the requeue list rather than by `wake()` draining `waiters` is exactly
            // the leak the branch below exists to prevent.
            if self.idle() == old_idle && !current_thread.exit_deliverable() {
                finish_blocking(guard);
            } else {
                // Memory became available before we decided to actually block, so we
                // never call finish_blocking() and thus never get removed from
                // `waiters` via the normal wake() path. Unlink ourselves here instead,
                // otherwise we leak a strong reference into `waiters` and a later,
                // unrelated wake() will try to reschedule us (or, if we've since
                // exited, a stale thread).
                //
                // Check is_linked() while holding the same lock wake() takes to drain
                // the list, so we can't race it: if wake() got here first, we're
                // already unlinked and this is a no-op.
                let mut waiters = self.waiters.lock();
                if current_thread.memwait_link.is_linked() {
                    unsafe {
                        waiters.cursor_mut_from_ptr(&**current_thread).remove();
                    }
                }
            }
            current_thread.set_state(ExecutionState::Running);
            current_thread.reset_sync_sleep_done();
        }
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        if current_thread.exit_deliverable() {
            // Our caller retries the allocation in a loop, so returning without blocking would
            // spin. Yield instead, and do it through the reinserting schedule -- that is one of the
            // two places MUST_EXIT is polled, so the thread takes its exit here rather than going
            // around again. If it holds mutexes, `maybe_exit` declines and we have at least given
            // up the cpu instead of burning it.
            schedule(SchedFlags::YIELD | SchedFlags::REINSERT);
        }
    }

    fn wake(&self) {
        let g = current_thread_ref().map(|ct| ct.enter_critical());
        // Take under the lock, requeue outside it -- see `Request::signal`, which is the same
        // shape for the same reason. Detaching the list is the claim: the blocking path above
        // unlinks itself under this same lock when it decides not to block, so a thread is either
        // in the list we just took or gone from it, never both.
        let waiters = self.waiters.lock().take();
        add_all_to_requeue(waiters);
        requeue_all();
        drop(g);
    }

    fn trigger_reclaim(&self) {
        if let Some(reclaim) = self.reclaim.poll() {
            // Only when the thread has something it can actually free.
            //
            // `reclaim_main`'s steps 1-5 are unimplemented, so the frames handed to it through
            // `reclaim()` are the only thing it can release -- and that producer signals for
            // itself. A pressure-driven wake therefore walks to `thisround == 0`, breaks, and
            // sleeps again, having preempted the caller at *donated REALTIME priority* to do it.
            // `should_reclaim` latches true for good once page data passes a third of memory, so
            // this fires on every allocation from that point on: measured at 361,690 wakes for the
            // 1.4M allocations of one zero-fill bench, against 0 in a boot that never latched, and
            // it is the whole of the residual isolated-vs-in-suite gap (2.39us vs 3.08us).
            //
            // F4b removed the 1000-round spin *inside* each wake; this removes the wake. When
            // steps 1-5 land, pressure becomes a reason to wake on its own again and this test has
            // to go -- it is a statement about what the thread can currently do, not about when
            // reclaim is wanted.
            if RECLAIM_NEEDS_WORK && reclaim.queued.load(Ordering::Relaxed) == 0 {
                return;
            }
            allocprofile::add(&allocprofile::RECLAIM_SIGNALS, 1);
            reclaim.cv.signal();
        } else {
            //logln!("warning -- cannot trigger reclaim thread before it is started");
        }
    }

    fn consider_reclaim(&self) {
        if self.should_reclaim() {
            self.trigger_reclaim();
        }
    }

    fn kern_cond(&self) -> bool {
        let idle = self.idle();
        let kern = self.kernel_used();
        let k2 = kern * 2;
        idle < k2
    }

    fn page_cond(&self) -> bool {
        let idle = self.idle();
        let page = self.page_data();
        let split_idle = idle / 2;
        page >= split_idle
    }

    fn should_reclaim(&self) -> bool {
        self.page_cond() || self.kern_cond()
    }

    fn idle(&self) -> usize {
        self.idle.load(Ordering::Acquire)
    }

    fn total(&self) -> usize {
        self.total.load(Ordering::Acquire)
    }

    fn kernel_used(&self) -> usize {
        self.kernel_used.load(Ordering::Acquire)
    }

    fn page_data(&self) -> usize {
        self.page_data.load(Ordering::Acquire)
    }

    fn allocated(&self) -> usize {
        self.allocated.load(Ordering::Acquire)
    }

    fn reclaimed(&self) -> usize {
        self.reclaimed.load(Ordering::Acquire)
    }

    fn freed(&self) -> usize {
        self.freed.load(Ordering::Acquire)
    }

    fn track_reclaimed(&self, count: usize) {
        self.reclaimed.fetch_add(count, Ordering::SeqCst);
    }

    fn track_frame_pager(&self, count: usize) {
        self.pager_outstanding.fetch_add(count, Ordering::SeqCst);
    }

    fn untrack_frame_pager(&self, count: usize) {
        self.pager_outstanding.fetch_sub(count, Ordering::SeqCst);
    }

    fn pager_outstanding(&self) -> usize {
        self.pager_outstanding.load(Ordering::SeqCst)
    }

    fn start_reclaim_thread(&self) {
        self.reclaim.call_once(|| ReclaimThread::new());
    }
}

pub static TRACKER: Once<MemoryTracker> = Once::new();

/// (idle, page_data, kernel_used, should_reclaim), in frames. For the perf marker.
pub fn tracker_snapshot() -> (usize, usize, usize, bool, usize) {
    let Some(t) = TRACKER.poll() else {
        return (0, 0, 0, false, 0);
    };
    (
        t.idle(),
        t.page_data(),
        t.kernel_used(),
        t.should_reclaim(),
        pooled_frames(),
    )
}

/// Frames parked in per-cpu pools right now. Charged to `page_data`/`kernel_used` like any other
/// allocated frame, so subtracting this is what separates pool occupancy from live use.
pub fn pooled_frames() -> usize {
    POOLED_FRAMES.load(Ordering::Relaxed) + framecache::cached_frames()
}

/// Fill in the tracker half of `MemoryStats`. The counters are read without a lock and so are not
/// mutually consistent; the sum invariant can be off by whatever raced. Consumers wanting a
/// coherent snapshot should compare successive samples, not audit one.
pub fn fill_stats(stats: &mut twizzler_abi::syscall::MemoryStats) {
    let Some(t) = TRACKER.poll() else {
        return;
    };
    stats.tracker = twizzler_abi::syscall::TrackerStats {
        idle: t.idle(),
        kernel_used: t.kernel_used(),
        page_data: t.page_data(),
        total: t.total(),
        pager_outstanding: t.pager_outstanding(),
        allocated: t.allocated(),
        freed: t.freed(),
        reclaimed: t.reclaimed(),
        waiting: t.waiting.load(Ordering::SeqCst),
        reclaiming: t.should_reclaim(),
        pooled: pooled_frames(),
    };
}

pub fn print_tracker_stats() {
    let tracker = TRACKER.poll().expect("page tracker not initialized");
    let total = tracker.total();
    let idle = tracker.idle();
    let kern = tracker.kernel_used();
    let page = tracker.page_data();
    let loan = tracker.pager_outstanding();
    logln!("memory status (in frames):");
    logln!(
        "       total: {} -- a: {} f: {} r: {}, {} waiters",
        total,
        tracker.allocated(),
        tracker.freed(),
        tracker.reclaimed(),
        tracker.waiting.load(Ordering::SeqCst)
    );
    logln!("        idle: {} {}%", idle, (idle * 100) / total);
    logln!("      kernel: {} {}%", kern, (kern * 100) / total);
    logln!(
        "        page: {} {}% ({} loaned)",
        page,
        (page * 100) / total,
        loan
    );
}

/// Allocate a physical frame. Flags specify zeroing, ownership tracking, and if waiting is okay.
///
/// The `flags` argument allows one to control if the resulting frame is
/// zeroed or not. Note that passing [FrameAllocFlags]::ZEROED guarantees that the returned frame
/// is zeroed, but the converse is not true.
///
/// The returned frame will have its ZEROED flag cleared. In the future, this will probably change
/// to reflect the correct state of the frame.
///
/// # Panic
/// Will panic if out of physical memory. For this reason, you probably want to use
/// [try_alloc_frame].
///
/// # Examples
/// ```
/// let uninitialized_frame = alloc_frame(FrameAllocFlags::empty());
/// let zeroed_frame = alloc_frame(FrameAllocFlags::ZEROED);
/// ```
pub fn alloc_frame(flags: FrameAllocFlags) -> FrameRef {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .alloc_frame(flags)
}

/// Try to allocate a physical frame. The flags argument is the same as in [alloc_frame]. Returns
/// None if no physical frame is available.
pub fn try_alloc_frame(flags: FrameAllocFlags, layout: Layout) -> Option<FrameRef> {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .try_alloc_frame(flags, layout)
}

/// Bulk counterpart of [`try_alloc_frame`]; see [`MemoryTracker::try_alloc_frames`].
pub fn try_alloc_frames(
    flags: FrameAllocFlags,
    layout: Layout,
    want: usize,
    out: &mut FrameStore,
) -> usize {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .try_alloc_frames(flags, layout, want, out)
}

/// Try to allocate a physical frame. The flags argument is the same as in [alloc_frame]. Returns
/// None if no physical frame is available. Splits the frame into children frames for the pager.
pub fn try_alloc_split_frames(flags: FrameAllocFlags, layout: Layout) -> Option<(FrameRef, usize)> {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .try_alloc_split_frames(flags, layout)
}
/// Free a physical frame.
///
/// If the frame's flags indicates that it is zeroed, it will be placed on
/// the zeroed list.
/// Free a frame.
///
/// **Must not synchronously take an object's page-table lock.** This used to be a performance
/// property; it became a correctness one when the object page-table guard started discharging its
/// deferred work on release (see `PtGuard`), because that work ends here and can run while a
/// *second* object's page-table lock is still held. `Mutex` is not reentrant, so a synchronous
/// acquire from this path would deadlock rather than merely be slow. Waking the reclaim thread is
/// fine; blocking on a page table is not.
pub fn free_frame(frame: FrameRef) {
    // The rc==0 assert below cannot catch a stale free of a frame parked rc=0 in a per-cpu
    // cache — the fa-bulk blind spot. This one can, and it fires in the SECOND freer's
    // backtrace, which is the one that names the bug.
    assert!(
        !frame.is_pooled(),
        "freeing frame that is parked in a per-cpu pool (double free): {:?}",
        frame
    );
    assert!(
        frame.refcount() == 0,
        "freeing frame with non-zero refcount"
    );
    assert!(
        !frame.is_pt(),
        "freeing frame that is still marked as a page table"
    );
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .free_frame(frame)
}

/// [`free_frame`] for a caller that is emptying a per-cpu cache and must not re-fill it.
/// Same asserts, same accounting; only the caching attempt is skipped.
///
/// `pub(crate)` for [`crate::memory::framecache`], whose trim and its two unreachable-in-practice
/// fallbacks are exactly this caller. The `is_pooled` assert applies: clear the bit before calling,
/// or the double-free tripwire fires on the cache's own drain.
pub(crate) fn free_frame_nopark(frame: FrameRef) {
    assert!(
        !frame.is_pooled(),
        "freeing frame that is parked in a per-cpu pool (double free): {:?}",
        frame
    );
    assert!(
        frame.refcount() == 0,
        "freeing frame with non-zero refcount"
    );
    assert!(
        !frame.is_pt(),
        "freeing frame that is still marked as a page table"
    );
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .free_frame_inner(frame, false)
}

/// Track a page as owned by the pager.
pub fn track_page_pager(count: usize) {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .track_frame_pager(count)
}

/// Track a page as owned by the pager.
pub fn untrack_page_pager(count: usize) {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .untrack_frame_pager(count)
}

/// Get outstanding pager pages
pub fn get_outstanding_pager_pages() -> usize {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .pager_outstanding()
}

/// Check if the system is low on memory
/// Deliberately still `should_reclaim()`, not [`memory_state`].
///
/// Switching it is tempting -- the band is cheaper to read and, unlike `page_cond`, it can
/// recover -- but it is not a free swap: `should_reclaim` latches true early in a boot, so
/// `background_zero_iter`'s bail-out is effectively permanent today, and moving this to the band
/// would silently *restart* background zeroing. That may well be an improvement; it is a
/// different change with its own measurement, and bundling it here would confound the A/B of
/// [`allocprofile::FA_FREE_TO_POOL`] with a resumed background worker.
pub fn is_low_mem() -> bool {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .should_reclaim()
}

pub fn get_waiting_threads() -> usize {
    TRACKER
        .poll()
        .map(|tracker| tracker.waiting.load(Ordering::SeqCst))
        .unwrap_or(0)
}

pub fn start_reclaim_thread() {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .start_reclaim_thread();
}

pub fn signal_waiters() {
    TRACKER.poll().expect("page tracker not initialized").wake();
}

/// Hand frames to the reclaim thread.
///
/// Blocks until that thread exists, so it must not be called from the allocator, a critical
/// section, or an interrupt. (Previously this spun on `Once::poll` and then `unwrap`ed, i.e. it
/// panicked outright if reclaim had not been started.)
pub fn reclaim(frames: impl IntoIterator<Item = FrameRef>) {
    let rt = TRACKER.poll().unwrap().reclaim.wait();
    let mut state = rt.state.lock();
    state.extend(frames);
    rt.queued.store(state.len(), Ordering::Relaxed);
    drop(state);
    // This is the wake that can do work, so it is never gated on `queued` -- it is what makes
    // `queued` nonzero.
    rt.cv.signal();
}

/// A/B knob for the gate in [MemoryTracker::trigger_reclaim]. `false` restores a signal on every
/// allocation once the reclaim latch trips.
const RECLAIM_NEEDS_WORK: bool = true;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct FrameAllocFlags: u32 {
        /// The page will be zeroed before returning.
        const ZEROED = 1;
        /// The page will be tracked as a kernel page.
        const KERNEL = 2;
        /// If no pages are available, wait.
        const WAIT_OK = 4;
    }
}

struct ReclaimThread {
    th: ThreadRef,
    state: Spinlock<Vec<FrameRef>>,
    /// `state.len()`, readable without the lock. Mirrored under it, so it is exact rather than
    /// advisory; [MemoryTracker::trigger_reclaim] consults it on every allocation and must not
    /// take a lock to do so.
    queued: AtomicUsize,
    cv: CondVar,
}

impl ReclaimThread {
    fn new() -> Self {
        extern "C" fn reclaim_start() {
            reclaim_main();
        }
        Self {
            th: start_new_kernel(Priority::BACKGROUND, reclaim_start, 0),
            state: Spinlock::new(Vec::new()),
            queued: AtomicUsize::new(0),
            cv: CondVar::new(),
        }
    }
}

#[allow(unused_assignments)]
#[allow(unused_variables)]
fn reclaim_main() {
    let tracker = TRACKER.poll().unwrap();
    // Blocks rather than spins: this thread is made runnable by `ReclaimThread::new()`, which has
    // not yet returned to `call_once`, so the value is never ready on the first look.
    let rt = tracker.reclaim.wait();
    let mut state = rt.state.lock();
    current_thread_ref()
        .unwrap()
        .donate_priority(Priority::REALTIME);
    const MAX_RECLAIM_ROUNDS: usize = 1000;
    const MAX_PER_ROUND: usize = 100;
    loop {
        let mut count = 0;
        let mut rounds = 0;
        allocprofile::add(&allocprofile::RECLAIM_WAKES, 1);
        tracker.note_idle_change();
        while tracker.should_reclaim() {
            allocprofile::add(&allocprofile::RECLAIM_ROUNDS, 1);
            let mut thisround = 0;
            /*
            0. Any directly passed pages-to-reclaim.
            1. Try to reclaim unused, backed object memory
            2. Try to reclaim rarely touched, backed object memory
            3. If should_reclaim because 2*k < idle, try to reclaim from kern alloc.
            4. If should_reclaim because page > idle / 2, then cache replacement clean objects.
            5. If pressure is high, cache replace any object.
            */
            while let Some(f) = state.pop() {
                free_frame(f);
                count += 1;
                thisround += 1;
                if thisround >= MAX_PER_ROUND {
                    break;
                }
            }
            // Mirrored under the same lock the pops happen under, so an allocator consulting it
            // lock-free never sees a count for frames this thread has already freed.
            rt.queued.store(state.len(), Ordering::Relaxed);

            if thisround < MAX_PER_ROUND {
                // TODO
            }

            // Nothing was reclaimable this round, so going around again cannot help: steps 1-5
            // above are unimplemented, and `state` is refilled only by another thread handing
            // frames over -- which a signal will announce.
            //
            // Without this the loop spun `MAX_RECLAIM_ROUNDS` times per wake at *realtime*
            // priority, and `should_reclaim` latches true for good once page data passes a third
            // of memory (`page_cond`), because nothing here ever brings it back down. Measured on
            // the sysbench suite: 49,638 wakes, 49.7 million rounds, and a zero-fill fault bench
            // whose own fault path accounted for 7% of its wall time -- the rest went to this
            // thread preempting it. See `sysbench.md` F4b.
            if thisround == 0 {
                break;
            }

            if rounds > MAX_RECLAIM_ROUNDS {
                break;
            }
            drop(state);
            log::trace!(
                "memory tracker should reclaim: {}, count={},thisround={},rounds={}",
                tracker.should_reclaim(),
                count,
                thisround,
                rounds,
            );
            schedule(SchedFlags::YIELD | SchedFlags::PREEMPT | SchedFlags::REINSERT);
            state = rt.state.lock();
            rounds += 1;
        }
        tracker.track_reclaimed(count);
        log::trace!(
            "memory tracker should reclaim: {}, count={}",
            tracker.should_reclaim(),
            count
        );
        if !tracker.should_reclaim() || count == 0 {
            state = rt.cv.wait(state);
        }
    }
}

pub fn init(total: usize, idle: usize, kern: usize) {
    // Provenance line for sweep logs: a log's allocator configuration must be decidable from the
    // log alone. It was not -- this printed two of the consts and stayed silent about the four
    // added later, so recovering the arm behind `many-poolval-98`'s numbers took a
    // counter-to-call-site trace instead of reading a line (see `zerofill.md` C3). Printed
    // unconditionally, with the value, rather than only when set: "enabled" appearing is evidence
    // only if its absence would have been evidence too, and a reader cannot tell a build that had
    // the flag off from a build that predates the flag.
    logln!(
        "allocprofile: BULK_PRECHARGE={} FA_FREE_TO_POOL={} FA_ALLOC_FROM_POOL={} FA_NO_TAKE={} FA_POOL_WATERMARK={} FA_POOL_BULK_REFILL={} FA_UNPARK_OVERLAP_CHECK={} TIME_ALLOCS={}",
        allocprofile::BULK_PRECHARGE,
        allocprofile::FA_FREE_TO_POOL,
        allocprofile::FA_ALLOC_FROM_POOL,
        allocprofile::FA_NO_TAKE,
        allocprofile::FA_POOL_WATERMARK,
        allocprofile::FA_POOL_BULK_REFILL,
        allocprofile::FA_UNPARK_OVERLAP_CHECK,
        allocprofile::TIME_ALLOCS,
    );
    TRACKER.call_once(|| MemoryTracker {
        kernel_used: AtomicUsize::new(kern),
        page_data: AtomicUsize::new(0),
        allocated: AtomicUsize::new(0),
        freed: AtomicUsize::new(0),
        reclaimed: AtomicUsize::new(0),
        waiting: AtomicUsize::new(0),
        idle: AtomicUsize::new(idle),
        total: AtomicUsize::new(total),
        pager_outstanding: AtomicUsize::new(0),
        reclaim: OnceWait::new(),
        waiters: Spinlock::new(LinkedList::new(LinkAdapter::NEW)),
    });
    // Derive the first band now that `total` exists, rather than leaving the empty initial
    // window to be discovered by whichever allocation happens first.
    TRACKER.poll().unwrap().note_idle_change();
}

const MAX_FA_FRAMES: usize = 32;

/// Most frames one steal may pop inside a single interrupts-off region. See [`FA_NO_TAKE`].
const MAX_UNPARK_BATCH: usize = 64;

/// Frames the thread-local pool keeps between operations.
///
/// The pool only ever grew before this: every operation's unused precharge merged back into it and
/// nothing ever returned a frame to the allocator, so a mapping-churn workload left 175,308 frames
/// -- about 700 MB -- parked in one thread's pool. That memory is charged as allocated, so the
/// tracker's own reclaim heuristics cannot see it as reclaimable, and `kernel_used` reached
/// 373,274 frames against 90,886 on a fresh boot (`sysbench.md` F4a).
///
/// Sized above the largest single precharge in the tree so that no caller re-allocates its surplus
/// on every call: `setup_cow_range` over a whole object asks for ~1030 (`max_number_new_tables` at
/// level 1 across MAX_SIZE, for both sides), and everything else asks for a handful.
const MAX_TLS_PRECHARGE: usize = 2048;

/// How many excess frames one drop returns. Bounded because a drop can run under an object's
/// page-table mutex, where a free loop over thousands of frames would hold it for milliseconds;
/// drops are frequent enough (one per mapping operation) that the pool converges in a few thousand
/// of them regardless.
const TRIM_PER_DROP: usize = 64;

/// Pool hysteresis. See [`allocprofile::FA_POOL_WATERMARK`]: without a low mark the pool has only
/// a ceiling, and a ceiling it is always sitting on is the same thing as having no room.
const POOL_HIGH_WATER: usize = MAX_TLS_PRECHARGE;
const POOL_LOW_WATER: usize = MAX_TLS_PRECHARGE / 8;

/// Frames taken per global-allocator acquisition when the pool has to be refilled. Sets the
/// fraction of allocations that touch the PFA lock: ~1 in `POOL_REFILL_BATCH`.
const POOL_REFILL_BATCH: usize = 64;

/// Slots in a per-cpu pool's buffer, allocated once per cpu by [`ensure_pool_provisioned`].
/// `MAX_TLS_PRECHARGE` is what `try_park` will fill it to; `MAX_FA_FRAMES` is the headroom
/// `merge` leaves for an abort list that has nowhere else to go.
const POOL_VEC_CAPACITY: usize = MAX_TLS_PRECHARGE + MAX_FA_FRAMES;

/// Inline capacity for a precharge list, spilling to the heap only when something asks for more.
///
/// `FA_NO_TAKE` hands out a *fresh* `FrameAllocator` per operation, so `precharge`'s eager reserve
/// was a kernel-heap allocation and free on **every** call -- measured at 133 ns of the create
/// path's 2,163 ns precharge, on a path that runs under the object page-table lock. The lock is
/// the second reason to remove it: `precharge`'s own comments document a self-deadlock from
/// allocating there while `allocate_chunk` holds `GLOBAL_PAGE_ALLOC`, so an allocation-free common
/// case removes a hazard, not just a cost.
///
/// **8, and there is no larger value that works** -- measured, not chosen. The buffer is
/// zero-initialized on every construction, and `map_page` constructs one per call, so the cost
/// scales with the capacity: `objdump` of `map_page` shows a second `memset` of exactly
/// `cap * 8 + 17` bytes beside a constant 138-byte one (that one is `Consistency`'s `TlbInvData`,
/// not this). Measured `take_fa`: **cap 8 -> 9 ns (no array memset at all), cap 16 -> 44 ns
/// (memset 145), cap 40 -> 51 ns (memset 337)**.
///
/// That is the whole tension: `precharge` reserves `count + MAX_FA_FRAMES` = **34**, so the create
/// path needs >= 34 to stop spilling, and anything >= 16 costs ~35-42 ns on *every* `map_page`
/// while the benefit lands only on calls that reach `precharge` -- 0.5% of them on the fault path.
/// At 8 the fault path avoids 97% of its kernel-heap allocations for free; the create path is
/// unchanged and cannot be helped from here.
///
/// **The way out is per-cpu, not per-operation.** A buffer owned by a `FrameCache` is constructed
/// once per cpu, so its zero-init is paid once instead of per `map_page`, and it can be as large
/// as the reserve wants. That is the argument for building one, and this const is the measurement
/// behind it.
///
/// Superseded reasoning, kept because it was wrong in an instructive way: **16 rather than 64**,
/// The allocator is constructed and returned *by value* on every `map_page`, which is the exact
/// cost `FA_NO_TAKE` exists to avoid -- `take_fa` fell 114 -> 9 ns by not moving a ~300-byte
/// allocator, and a 64-slot inline array would put 512 bytes straight back. A per-op allocator
/// holds `count` in the common case (2 on the create path, 4 on the fault path); the paths that
/// hold more -- a 64-frame refill surplus, `setup_cow_range`'s ~1030 -- spill, and each already
/// costs far more than one allocation. `FA_SPILL` is what says whether 16 was the right guess; if
/// it is not small, raise this rather than defend it.
const FA_INLINE_CAP: usize = 8;

/// A frame list that lives inline until it outgrows [`FA_INLINE_CAP`].
///
/// Invariant: exactly one side holds frames. Unspilled, everything is in `inline` and `heap` has
/// no capacity; spilled, everything is in `heap` and `inline` is empty. `capacity()` reports the
/// true bound either way, which is what the callers that *must not allocate* --
/// `park_frame_in_pool` and `raw_alloc_frames` -- already check before pushing.
pub struct FrameStore {
    inline: heapless::Vec<FrameRef, FA_INLINE_CAP>,
    heap: alloc::vec::Vec<FrameRef>,
    spilled: bool,
}

impl FrameStore {
    /// **Not `const`**, deliberately. As a `const fn` this whole aggregate is const-evaluable, and
    /// LLVM materialised it as a zeroed constant: `objdump` of `map_page` showed a 138-byte
    /// `memset` inside the `take_fa` span, worth 36 ns on *every* call. `abort`, a bare
    /// `heapless::Vec` field constructed the same way, is 256 bytes and is **not** memset -- so the
    /// zeroing is not heapless's doing (its `INIT` is commented "important for optimization of
    /// `new`") but this constructor's const-evaluability. Check the disassembly, not the source,
    /// before making it `const` again.
    pub fn new() -> Self {
        Self {
            inline: heapless::Vec::new(),
            heap: alloc::vec::Vec::new(),
            spilled: false,
        }
    }

    /// A heap-backed store with room for `cap`, for the per-cpu pool, which needs
    /// `MAX_TLS_PRECHARGE` and is provisioned once per cpu from a context that may allocate.
    pub fn with_heap_capacity(cap: usize) -> Self {
        let mut heap = alloc::vec::Vec::new();
        heap.reserve_exact(cap);
        Self {
            inline: heapless::Vec::new(),
            heap,
            spilled: true,
        }
    }

    pub fn len(&self) -> usize {
        if self.spilled {
            self.heap.len()
        } else {
            self.inline.len()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        if self.spilled {
            self.heap.capacity()
        } else {
            FA_INLINE_CAP
        }
    }

    /// Move to the heap with room for `cap`. **Allocates**, so every caller that reaches this must
    /// be one that may.
    fn spill(&mut self, cap: usize) {
        if self.spilled {
            self.heap.reserve(cap.saturating_sub(self.heap.len()));
            return;
        }
        allocprofile::add(&allocprofile::FA_SPILL, 1);
        let mut heap = alloc::vec::Vec::new();
        heap.reserve_exact(cap.max(self.inline.len()));
        heap.extend(self.inline.drain(..));
        self.heap = heap;
        self.spilled = true;
    }

    pub fn push(&mut self, frame: FrameRef) {
        if self.spilled {
            self.heap.push(frame);
            return;
        }
        if self.inline.push(frame).is_err() {
            // Only reachable from a caller that did not check `capacity()` first, i.e. one that
            // is allowed to allocate.
            self.spill(FA_INLINE_CAP * 2);
            self.heap.push(frame);
        }
    }

    pub fn pop(&mut self) -> Option<FrameRef> {
        if self.spilled {
            self.heap.pop()
        } else {
            self.inline.pop()
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        if self.len() + additional <= self.capacity() {
            return;
        }
        self.spill(self.len() + additional);
    }

    pub fn clear(&mut self) {
        if self.spilled {
            self.heap.clear()
        } else {
            self.inline.clear()
        }
    }

    pub fn extend(&mut self, iter: impl Iterator<Item = FrameRef>) {
        for frame in iter {
            self.push(frame);
        }
    }

    /// Drain every frame, leaving the store empty. Takes all of them rather than a range: the
    /// only callers want the whole list, and a partial drain across two backings has no cheap
    /// representation.
    pub fn drain_all(&mut self) -> impl Iterator<Item = FrameRef> + '_ {
        let spilled = self.spilled;
        let heap = &mut self.heap;
        let inline = &mut self.inline;
        let a = if spilled { Some(heap.drain(..)) } else { None };
        let b = if spilled {
            None
        } else {
            Some(inline.drain(..))
        };
        a.into_iter().flatten().chain(b.into_iter().flatten())
    }

    pub fn iter(&self) -> core::slice::Iter<'_, FrameRef> {
        self.as_slice().iter()
    }

    pub fn as_slice(&self) -> &[FrameRef] {
        if self.spilled {
            &self.heap
        } else {
            &self.inline
        }
    }
}

impl core::ops::Index<usize> for FrameStore {
    type Output = FrameRef;
    fn index(&self, i: usize) -> &FrameRef {
        if self.spilled {
            &self.heap[i]
        } else {
            &self.inline[i]
        }
    }
}

impl core::ops::IndexMut<usize> for FrameStore {
    fn index_mut(&mut self, i: usize) -> &mut FrameRef {
        if self.spilled {
            &mut self.heap[i]
        } else {
            &mut self.inline[i]
        }
    }
}

/// Return leftover precharge to the cache **clean** instead of dirty.
///
/// Measured cause of the `object_map_unmap_syscall` regression in the framecache ship arms
/// (+6.6-7.1%, disjoint from both baseline arms). That bench precharges two page-table frames per
/// op and consumes neither -- the context's tables already exist -- so the frames cycle
/// allocator -> cache -> allocator forever. Returning them dirty makes the cache memset a frame it
/// is about to hand straight back: `zeroed_inline=1,859,632` of `1,860,000` hand-outs, 99.98%,
/// ~2 per op. The old per-cpu pool never paid this (`pool-zeroed=0` over 2.12M frames) because
/// `finish_parked_alloc` zeroes only when `was_parked`, and its comment states the rule this
/// restores: "A precharged one was allocated zeroed and nobody has written it."
const PRECHARGE_RETURNS_CLEAN: bool = true;

pub struct FrameAllocator {
    flags: FrameAllocFlags,
    layout: Layout,
    abort: heapless::Vec<FrameRef, MAX_FA_FRAMES>,
    precharge: FrameStore,
    avoid_alloc: bool,
    /// Whether everything still in `precharge` is known to be all-zero.
    ///
    /// True when this allocator asks for `ZEROED` frames, because a frame that is still in the
    /// precharge list was never popped by `try_allocate` and so was never handed to anyone who
    /// could write it. Cleared by [`Self::merge`], which can move *abort* frames into the
    /// precharge list -- those went out to a failed map and may have been written.
    precharge_known_zero: bool,
}

impl FrameAllocator {
    pub fn new(flags: FrameAllocFlags, layout: Layout) -> Self {
        FrameAllocator {
            flags,
            layout,
            abort: heapless::Vec::new(),
            precharge: FrameStore::new(),
            avoid_alloc: false,
            precharge_known_zero: flags.contains(FrameAllocFlags::ZEROED),
        }
    }

    pub fn merge(&mut self, other: &mut Self) {
        // Conservative and unconditional: this function can move `other.abort` into
        // `self.precharge`, and an abort frame went out to a map that failed, so it may have been
        // written. Narrowing this to the branch that actually does it would be an invariant
        // nobody re-checks when the branches change.
        self.precharge_known_zero = false;
        // Take the other's list wholesale when we have none of our own, rather than copying it
        // into ours. This is the path every `take_or_new_frame_allocator` returns through: the
        // thread-local pool is moved out at the start of an operation and handed back by
        // `save_frame_allocator` to a *freshly constructed* allocator, so the destination is
        // empty essentially every time and `append` was a reserve-and-memcpy of the whole pool.
        //
        // That pool is not small. It only ever grows -- nothing trims it -- and after a workload
        // that churns mappings it was measured at 37,001 frames, making this a ~300 KB copy on
        // every page installed by a fault: 14.2 us of the 15.2 us a `map_page` cost, against
        // 0.33 us on a fresh boot where the same pool holds 1.6 frames.
        // **This must not allocate.** `merge` reaches here from `FrameAllocator::drop`, and a
        // dropped allocator can be one `GlobalPageAlloc::extend` built *while `allocate_chunk`
        // holds `GLOBAL_PAGE_ALLOC`* -- growing a vec there self-deadlocks a non-reentrant
        // spinlock, and worse, a heap extension re-enters `save_frame_allocator` and takes a
        // second `&mut` to this same pool from `tls_fa()` while this borrow is live.
        //
        // Measured before this was bounded: `grew-append=2,237` in one `sysbench` boot. Note the
        // count understates it -- `Vec` growth is amortized, so "exceeds capacity constantly and
        // doubles a handful of times" and "rarely exceeds capacity" produce the same small
        // number. `grew-extend=0` over 2.06M saves is what says the abort line is not the one.
        //
        // Side effect worth knowing: bounding here subsumes `trim`'s over-cap job. Excess is
        // returned at the moment it would have pushed the pool past `MAX_TLS_PRECHARGE`, so the
        // pool sits at its cap by construction instead of converging 64 frames per drop, and
        // `trim` is left with the pressure-driven targets its doc comment describes.
        // A swap moves the vec wholesale and cannot allocate, so it needs no bound -- but it also
        // hands the *destination's* buffer to `other`, and under [`allocprofile::FA_NO_TAKE`] the
        // destination is the per-cpu pool while `other` is a short-lived per-operation allocator.
        // Swapping there replaces a provisioned 2,080-slot pool with a two-slot one, permanently:
        // nothing on the park path may allocate, so the pool can never grow back. That is the
        // coupling behind three separate `FA_PARK_NO_CAP` blowups today (`resfix` 1.5M, `exactpc`
        // 1.24M, `consfast` 996k) -- each time, something stopped calling `precharge` with a
        // pool-sized reserve and the pool quietly lost its capacity.
        //
        // Guarded on capacity rather than on the const: take the swap only when it does not
        // *downgrade* the destination. Under the take/save design the destination is a fresh
        // allocator (capacity 0) and the source is the pool, so the swap still happens exactly
        // as before.
        if self.precharge.is_empty() && self.precharge.capacity() < other.precharge.capacity() {
            core::mem::swap(&mut self.precharge, &mut other.precharge);
        } else {
            allocprofile::add(&allocprofile::FA_SAVE_APPEND, 1);
            let cap = self.precharge.capacity();
            // Take only what fits, keeping `MAX_FA_FRAMES` in reserve for the abort list below,
            // which has nowhere else to go. What is left stays in `other` and is freed by
            // `Drop`'s `clear()` -- outside this region, by a path that cannot allocate.
            let room = self
                .precharge
                .capacity()
                .saturating_sub(MAX_FA_FRAMES)
                .min(MAX_TLS_PRECHARGE)
                .saturating_sub(self.precharge.len());
            let take = room.min(other.precharge.len());
            for _ in 0..take {
                let Some(frame) = other.precharge.pop() else {
                    break;
                };
                self.precharge.push(frame);
            }
            let left = other.precharge.len() as u64;
            if left > 0 {
                allocprofile::add(&allocprofile::FA_MERGE_LEFTOVER, left);
            }
            debug_assert_eq!(self.precharge.capacity(), cap);
            if self.precharge.capacity() != cap {
                allocprofile::add(&allocprofile::FA_SAVE_GREW_APPEND, 1);
            }
        }
        // Bounded at `MAX_FA_FRAMES` and unconditional: abort frames can carry a non-zero
        // refcount, and `free_frame` asserts against that, so they must be recycled rather than
        // dropped on the floor. The headroom above is what keeps this from growing the vec.
        let cap = self.precharge.capacity();
        self.precharge.extend(other.abort.drain(..));
        if self.precharge.capacity() != cap {
            allocprofile::add(&allocprofile::FA_SAVE_GREW_EXTEND, 1);
        }
    }

    #[track_caller]
    pub fn precharge(&mut self, count: usize, flags: FrameAllocFlags) {
        if count >= PHYS_LEVEL_LAYOUTS[1].size() / PHYS_LEVEL_LAYOUTS[0].size() {
            // debug!, not warn!: this fires ~1600 times a sweep on healthy runs, which drowns real
            // warnings in grep-based triage. Raise it again if it ever correlates with a failure.
            log::debug!(
                "frame allocator precharge: requested {} frames at {} (have {})",
                count,
                core::panic::Location::caller(),
                self.precharge.len()
            );
        }
        allocprofile::add(&allocprofile::PRECHARGE_CALLS, 1);
        crate::obj::pagetables::mapprobe::tick(&crate::obj::pagetables::mapprobe::PC_CALLS);
        // **Eager, i.e. before the early return.** The free path may never grow this vec, so it
        // declines into whatever capacity the last *slow* precharge happened to leave. With the
        // reserve behind the early return, 77% of calls returned without ever topping the
        // capacity up, and the pool's capacity sat permanently behind its cap: `no-cap=2,024,112`
        // declines a boot at cap 2048, and *rising* to 2,514,102 at cap 16384 -- capacity, not
        // depth, becoming the binding constraint precisely because it is reserved lazily
        // (faplan.md, cap A/B). Reserving here costs a capacity check on the fast path and no
        // allocation once the pool is at size.
        // Pool-sized only for the allocator that *is* the pool. Under [`allocprofile::FA_NO_TAKE`]
        // every operation gets a fresh allocator with an empty `Vec`, so this reserve stops being
        // a capacity check and becomes a **16,640-byte kernel-heap allocation and free per
        // `map_page`** -- 2,080 slots for a request of two. Measured across the flip
        // (`mapprobe1`/`knobs-on`, isolated `page_fault_zero_fill`): `precharge` 35 ns -> 620 ns
        // while `take_fa` fell 114 -> 9 as designed. The eager reserve's own justification is
        // about the *pool's* capacity, which the free path can never grow itself, and this
        // allocator is not it.
        //
        // What the pool's capacity then depends on: `merge` swaps our vec into an empty TLS slot,
        // so the pool inherits whatever we sized. `FA_PARK_NO_CAP` (`no-cap=` on `PERFMARK-PARK`)
        // is the counter that says whether that is enough; it was 0 with the pool-sized reserve
        // and must be watched here.
        if framecache::ENABLED {
            // **Exactly `count`, and no `ensure_pool_provisioned`.** Both differences are the
            // point of the cache owning the storage instead of this allocator.
            //
            // The `+ MAX_FA_FRAMES` below is headroom for `merge` to fold the abort list into the
            // per-cpu pool from `Drop`; with the cache there is no `merge` -- `Drop` hands surplus
            // straight back -- so the headroom buys nothing and costs everything. `FA_INLINE_CAP`
            // is 8 and every hot-path caller asks for 1-4 (`tables_needed` with `PRECHARGE_EXACT`
            // on the create path, the fault path's handful), so at `count` the reserve is a
            // capacity check against inline storage and reaches the kernel heap not at all --
            // against 34 slots, which spills on *every* call, measured at 133 ns of a 2,163 ns
            // precharge on a path that runs under the object page-table lock.
            //
            // `setup_cow_range`'s ~1,030 still spills, once, and is left to: it is rare, and each
            // such call already costs orders more than one allocation. Expressing it in whole
            // magazines is Part 3's D4 and is not built.
            let t_res = crate::obj::pagetables::mapprobe::start();
            self.precharge
                .reserve(count.saturating_sub(self.precharge.len()));
            crate::obj::pagetables::mapprobe::record(
                &crate::obj::pagetables::mapprobe::PC_RESERVE_NS,
                t_res,
            );
        } else if allocprofile::FA_FREE_TO_POOL {
            if allocprofile::FA_NO_TAKE {
                // The pool is provisioned once per cpu, by the function below, and this allocator
                // is not it: reserve for what this operation will hold. Sizing it pool-wide here
                // was a 16,640-byte kernel-heap allocation and free on **every** call, because
                // `FA_NO_TAKE` hands out a fresh allocator with an empty `Vec` each time.
                let t_prov = crate::obj::pagetables::mapprobe::start();
                ensure_pool_provisioned();
                crate::obj::pagetables::mapprobe::record(
                    &crate::obj::pagetables::mapprobe::PC_PROV_NS,
                    t_prov,
                );
                let t_res = crate::obj::pagetables::mapprobe::start();
                self.precharge
                    .reserve((count + MAX_FA_FRAMES).saturating_sub(self.precharge.len()));
                crate::obj::pagetables::mapprobe::record(
                    &crate::obj::pagetables::mapprobe::PC_RESERVE_NS,
                    t_res,
                );
            } else {
                // Take/save design: this allocator *is* the pool, so the reserve is the pool's.
                self.precharge.reserve(
                    (MAX_TLS_PRECHARGE + MAX_FA_FRAMES).saturating_sub(self.precharge.len()),
                );
            }
        }
        if self.precharge.len() >= count {
            allocprofile::add(&allocprofile::PRECHARGE_EARLY, 1);
            return;
        }
        // Headroom for the free path, taken here and nowhere else. `park_frame_in_pool` may never
        // allocate, so it can only push into capacity someone else reserved -- and reserving
        // `count` alone is a handful of frames, which is why parking peaked at 299 against a 2048
        // bound and the first A/B measured a nearly inert feature (faplan.md RESULT).
        //
        // **It must be here rather than in `Drop`.** A dropped `FrameAllocator` can be one that
        // `GlobalPageAlloc::extend` built *while `allocate_chunk` holds `GLOBAL_PAGE_ALLOC`*, so
        // reserving there re-enters the kernel heap and self-deadlocks on a non-reentrant
        // spinlock -- deterministically, at the first heap extension, which is early enough to
        // hang during secondary-cpu enumeration. This line, by contrast, is a reserve the
        // function already performed, so asking for more capacity cannot introduce an allocation
        // context that was not already there.
        // The pool arm already reserved above, eagerly; this is the plain path.
        // (`+ MAX_FA_FRAMES` there so the abort list always has somewhere to go: `merge` bounds
        // the precharge move to `MAX_TLS_PRECHARGE`, leaving that headroom free, and abort frames
        // *must* be recycled rather than freed -- they can carry a non-zero refcount, which
        // `free_frame` asserts against.)
        if !allocprofile::FA_FREE_TO_POOL {
            self.precharge.reserve(count);
        }
        let all_flags = self.flags | flags;
        let mut remaining = count - self.precharge.len();
        if allocprofile::BULK_PRECHARGE {
            // One acquisition for the batch.
            let t_fetch = crate::obj::pagetables::mapprobe::start();
            let got = try_alloc_frames(all_flags, self.layout, remaining, &mut self.precharge);
            crate::obj::pagetables::mapprobe::record(
                &crate::obj::pagetables::mapprobe::PC_FETCH_NS,
                t_fetch,
            );
            allocprofile::add(&allocprofile::PRECHARGE_FETCHED, got as u64);
            // `saturating_sub`, not `-`: with [`allocprofile::FA_POOL_BULK_REFILL`] the batch may
            // deliberately return **more** than was asked for. `[profile.release]` leaves
            // overflow checks off, so a plain subtraction would wrap to `usize::MAX` here and the
            // loop below would try to allocate the machine.
            remaining = remaining.saturating_sub(got);
        }
        // The bulk path never waits, so a short return still has to honour `WAIT_OK` -- which only
        // the singular call implements. Rare by construction: it means memory ran out mid-batch.
        for _ in 0..remaining {
            let Some(frame) = try_alloc_frame(all_flags, self.layout) else {
                return;
            };
            allocprofile::add(&allocprofile::PRECHARGE_FETCHED, 1);
            self.precharge.push(frame);
        }
    }

    /// Precharge without waiting, returning how many frames are now held.
    ///
    /// A caller that already holds a lock can try to get its frames without giving the lock up,
    /// and find out cheaply whether it has to: waiting for memory is what must not happen under a
    /// lock, and in the common case there is nothing to wait for.
    #[track_caller]
    pub fn precharge_nowait(&mut self, count: usize) -> usize {
        allocprofile::add(&allocprofile::PRECHARGE_CALLS, 1);
        // Hooked here as well as in `precharge`: with an exact page-table precharge, `map_page`
        // often makes no request at all, and the fill loop's `precharge_nowait` becomes the only
        // allocating call a fault path makes. Provisioning must not depend on which one runs.
        if !framecache::ENABLED && allocprofile::FA_FREE_TO_POOL && allocprofile::FA_NO_TAKE {
            ensure_pool_provisioned();
        }
        if self.precharge.len() >= count {
            allocprofile::add(&allocprofile::PRECHARGE_EARLY, 1);
        }
        let want = count.saturating_sub(self.precharge.len());
        if allocprofile::BULK_PRECHARGE && want > 0 {
            self.precharge.reserve(want);
            let got = try_alloc_frames(
                self.flags & !FrameAllocFlags::WAIT_OK,
                self.layout,
                want,
                &mut self.precharge,
            );
            allocprofile::add(&allocprofile::PRECHARGE_FETCHED, got as u64);
        }
        while self.precharge.len() < count {
            let Some(frame) = try_alloc_frame(self.flags & !FrameAllocFlags::WAIT_OK, self.layout)
            else {
                break;
            };
            allocprofile::add(&allocprofile::PRECHARGE_FETCHED, 1);
            self.precharge.push(frame);
        }
        self.precharge.len()
    }

    #[track_caller]
    pub fn try_allocate(&mut self) -> Option<FrameRef> {
        // One exit, because both pool paths can hand back a frame that does not yet match what
        // this allocator's flags promise: `abort` holds frames a failed map gave back, and
        // `precharge` now also holds frames parked straight off the free path.
        let frame = if !self.abort.is_empty() {
            self.abort.pop()
        } else if self.precharge.len() == 0 {
            allocprofile::add(&allocprofile::FA_ALLOC_GLOBAL, 1);
            if self.avoid_alloc {
                allocprofile::add(&allocprofile::FA_ALLOC_AVOID_EMPTY, 1);
                log::warn!(
                    "frame allocator out of precharged frames and avoid_alloc is set, from {}",
                    core::panic::Location::caller()
                );
                crate::panic::backtrace(true, None);
                try_alloc_frame(self.flags & !FrameAllocFlags::WAIT_OK, self.layout)
            } else {
                try_alloc_frame(self.flags, self.layout)
            }
        } else {
            allocprofile::add(&allocprofile::FA_ALLOC_POOL, 1);
            self.precharge.pop()
        }?;
        Some(self.finish_pool_alloc(frame))
    }

    /// Bring a frame taken from this allocator up to what its flags promise.
    ///
    /// Parked frames arrive dirty and still charged to their last owner's class; frames from the
    /// global path already satisfy both, so for them this is a pair of predictable-branch tests.
    fn finish_pool_alloc(&self, frame: FrameRef) -> FrameRef {
        finish_parked_alloc(frame, self.flags)
    }
}

/// Which side of [`framecache`] a request should prefer.
///
/// `ZEROED` absent means the caller overwrites the page before reading it -- `UninitPageProvider`
/// for kernel-heap growth, and `Frame::cow_frame`, which `copy_contents_from`s the whole 4 KiB
/// immediately. Serving those from the dirty side skips a memset *and* leaves a zeroed frame for
/// a caller that actually needs one, which is two wins from one branch.
fn want_of(flags: FrameAllocFlags) -> framecache::Want {
    if flags.contains(FrameAllocFlags::ZEROED) {
        framecache::Want::Zeroed
    } else {
        framecache::Want::Any
    }
}

/// Bring a frame from [`framecache`] up to what `flags` promise.
///
/// Split from [`finish_parked_alloc`] rather than sharing it, for one reason worth stating: that
/// function decides whether to zero by asking whether the frame was `POOLED`, because the old pool
/// has no way to know whether a given frame is dirty. The cache does know -- that is what its
/// clean/dirty split *is* -- so it passes the answer in, and the frames it says are clean cost no
/// memset at all. Folding the two would put that decision back on a bit that cannot carry it.
///
/// **Must run with interrupts enabled**: the zeroing below is a 4 KiB memset.
fn finish_cached_alloc(frame: FrameRef, flags: FrameAllocFlags, needs_zeroing: bool) -> FrameRef {
    if allocprofile::FA_UNPARK_OVERLAP_CHECK {
        check_overlap(frame, "framecache");
    }
    // The gauge decrement happened in the cache; this only clears the tripwire bit. Splitting them
    // is deliberate -- the cache knows its own depth, and having two owners increment one counter
    // is how the old pool's accounting became unreadable.
    frame.clear_pooled();
    if needs_zeroing {
        debug_assert!(flags.contains(FrameAllocFlags::ZEROED));
        frame.zero();
        // Same postcondition `finish_raw_alloc` asserts after its own zeroing. Without it a cache
        // hand-out has none at all, and "dirty frame served as zeroed" is precisely what panicked
        // the per-cpu cache arm before this one.
        assert!(
            frame.is_zeroed(),
            "framecache hand-out not zeroed after zero(): {:?}",
            frame
        );
        frame.set_not_zero();
        allocprofile::add(&allocprofile::FA_POOL_ZEROED, 1);
    }
    let want_kernel = flags.contains(FrameAllocFlags::KERNEL);
    if frame.is_kernel() != want_kernel {
        // The charge moves at hand-out rather than at cache entry, so caching cannot
        // systematically drain `page_data` into `kernel_used` -- those two are what the leak
        // harness watches, and `trk.pooled` is what lets it subtract the rest.
        let tracker = TRACKER.poll().expect("page tracker not initialized");
        if want_kernel {
            tracker.page_data.fetch_sub(1, Ordering::SeqCst);
            tracker.kernel_used.fetch_add(1, Ordering::SeqCst);
        } else {
            tracker.kernel_used.fetch_sub(1, Ordering::SeqCst);
            tracker.page_data.fetch_add(1, Ordering::SeqCst);
        }
        frame.set_kernel(want_kernel);
    }
    frame
}

/// Offer a freed level-0 frame to [`framecache`], applying the same admission rules the old pool
/// applies. Returns whether the cache took it.
///
/// The `POOLED` bit and the two tripwires are set *here* rather than inside the cache, so that the
/// double-free detector and the overlap check live on the one path every free goes through
/// regardless of which cache is enabled. `framecache` is a container; the invariants are the
/// tracker's.
fn cache_freed_frame(frame: FrameRef) -> bool {
    cache_freed_frame_hinted(frame, false)
}

/// [`cache_freed_frame`], carrying the caller's guarantee that the frame is already all-zero.
fn cache_freed_frame_hinted(frame: FrameRef, known_zero: bool) -> bool {
    if !framecache::ENABLED || !tls_ready() {
        return false;
    }
    // Pressure is where caching stops: a cached frame is invisible to the physical allocator, and
    // reclaim relies on frees actually returning memory.
    if memory_state() >= MemoryState::Tight {
        allocprofile::add(&allocprofile::FA_PARK_PRESSURE, 1);
        return false;
    }
    assert!(
        !frame.is_wired(),
        "caching a wired frame (raw_free_frame would have caught this): {:?}",
        frame
    );
    check_overlap(frame, "framecache-free");
    frame.set_cow(false);
    assert!(
        !frame.mark_pooled(),
        "frame already in a per-cpu cache at free (double free): {:?}",
        frame
    );
    if framecache::free_one_hinted(frame, known_zero) {
        return true;
    }
    // Refused -- the cache is at its bound, or off. Undo the bit so the caller's path to the
    // physical allocator sees an ordinary frame; `free_frame_nopark`'s own assert would fire on it
    // otherwise, which would turn the pressure valve into a panic.
    frame.clear_pooled();
    false
}

/// The body of [`FrameAllocator::finish_pool_alloc`], as a free function so the global entry
/// points can serve a pooled frame under the same rules. **Must run with interrupts enabled**:
/// the zeroing below is a 4 KiB memset.
fn finish_parked_alloc(frame: FrameRef, flags: FrameAllocFlags) -> FrameRef {
    {
        // Only a frame that came off the *free* path is dirty. A precharged one was allocated
        // zeroed and nobody has written it -- `is_zeroed()` cannot tell them apart, because
        // `finish_raw_alloc` clears that flag at every hand-out, so testing it here would
        // re-zero the whole pool: a 4 KiB memset per page-table allocation that does not happen
        // today. The POOLED bit is exactly the distinction.
        // Parity with `finish_raw_alloc`, which this path skips. Gated separately -- see
        // [`allocprofile::FA_UNPARK_OVERLAP_CHECK`]; it cannot ride in an arm that never unparks.
        if allocprofile::FA_UNPARK_OVERLAP_CHECK {
            check_overlap(frame, "unpark");
        }
        let was_parked = frame.clear_pooled();
        if was_parked {
            POOLED_FRAMES.fetch_sub(1, Ordering::Relaxed);
        }
        if was_parked && flags.contains(FrameAllocFlags::ZEROED) {
            // Page-table code trusts the request flag and parses whatever is there as entries.
            // Skipping this is what produced the smoke-boot panic in the abandoned cache arm.
            frame.zero();
            // `finish_raw_alloc` asserts this after its own zeroing; without it a pool hand-out
            // has no postcondition at all, and "dirty frame served as zeroed" is exactly what
            // panicked the abandoned per-cpu-cache arm.
            assert!(
                frame.is_zeroed(),
                "pool hand-out not zeroed after zero(): {:?}",
                frame
            );
            frame.set_not_zero();
            allocprofile::add(&allocprofile::FA_POOL_ZEROED, 1);
        }
        let want_kernel = flags.contains(FrameAllocFlags::KERNEL);
        if frame.is_kernel() != want_kernel {
            // The charge moves here rather than at park, so parking cannot systematically drain
            // `page_data` into `kernel_used` -- those two are what the leak harness watches.
            let tracker = TRACKER.poll().expect("page tracker not initialized");
            if want_kernel {
                tracker.page_data.fetch_sub(1, Ordering::SeqCst);
                tracker.kernel_used.fetch_add(1, Ordering::SeqCst);
            } else {
                tracker.kernel_used.fetch_sub(1, Ordering::SeqCst);
                tracker.page_data.fetch_add(1, Ordering::SeqCst);
            }
            frame.set_kernel(want_kernel);
        }
        frame
    }
}

impl FrameAllocator {
    /// Take frames back that an operation allocated and did not use.
    ///
    /// These are marked pooled for the same reason parked frames are: **they can be dirty**.
    /// `Frame::cow_frame` aborts a frame it has already `copy_contents_from`'d into, so an
    /// aborted frame can hold a copy of another page. `try_allocate` returns the abort list
    /// *first*, and `populate` installs what it gets as a page table without zeroing it, trusting
    /// the frame to be clean — so without this, a failed COW can hand a page of someone else's
    /// data to the page-table code to parse as entries. That is a live bug independent of this
    /// change; parking only makes the pool it hides in bigger and longer-lived.
    pub fn abort(&mut self, frames: impl IntoIterator<Item = FrameRef>) {
        for frame in frames {
            if !frame.mark_pooled() {
                POOLED_FRAMES.fetch_add(1, Ordering::Relaxed);
            }
            if self.abort.push(frame).is_err() {
                // Dropped on the floor, as before -- but keep the gauge honest about it.
                if frame.clear_pooled() {
                    POOLED_FRAMES.fetch_sub(1, Ordering::Relaxed);
                }
                log::warn!(
                    "frame allocator abort: too many frames to store, dropping frame {:?}",
                    frame
                );
            }
        }
    }

    /// # Known gap: this frees abort frames, which `Drop`'s comment says must not be freed
    ///
    /// Three sites, two files, pairwise plausible and jointly contradictory:
    /// `Drop` states abort frames "can carry a non-zero refcount (a failed map after an rc bump)"
    /// and must be recycled rather than freed; this loop `free_frame`s them; and `free_frame`
    /// `assert!`s `refcount() == 0` -- live in release, since `[profile.release]` sets only
    /// `debug = true`.
    ///
    /// **Reachability is narrow, and two of the three routes are already closed.** All four
    /// `abort()` call sites in the tree are level-0 (`obj/data.rs:525/560/901`, `frame.rs:1046`),
    /// so a level-1 allocator's abort list is always empty -- `obj/data.rs:455` propagates with
    /// `?` and never aborts. And `save_frame_allocator` is now infallible, so on the level-0 path
    /// `merge` always drains abort before this runs. What remains is the `!tls_ready()` early-boot
    /// branch with a level-0 allocator that aborted after an rc bump.
    ///
    /// Pre-existing; documented rather than fixed so it is not re-derived from two files. See
    /// faplan.md.
    pub fn clear(&mut self) {
        while let Some(frame) = self.abort.pop() {
            if frame.clear_pooled() {
                POOLED_FRAMES.fetch_sub(1, Ordering::Relaxed);
            }
            free_frame_nopark(frame);
        }
        while let Some(frame) = self.precharge.pop() {
            if frame.clear_pooled() {
                POOLED_FRAMES.fetch_sub(1, Ordering::Relaxed);
            }
            free_frame_nopark(frame);
        }
    }

    /// Return up to [`TRIM_PER_DROP`] frames held above [`MAX_TLS_PRECHARGE`] to the allocator.
    ///
    /// Runs before the pool goes back to thread-local storage, so the frames it gives up are ones
    /// no operation asked for.
    fn trim(&mut self) {
        // Now that the free path parks here, the pool sits at its bound rather than drifting up
        // to it, so this is what returns memory as pressure rises. Draining by attrition on the
        // local cpu -- allocators are dropped constantly -- avoids needing cross-cpu access to
        // per-cpu pools, which is the one thing the ownership rules here cannot give.
        let target = match memory_state() {
            MemoryState::Plenty => MAX_TLS_PRECHARGE,
            MemoryState::Loaded => MAX_TLS_PRECHARGE / 4,
            MemoryState::Tight => MAX_TLS_PRECHARGE / 16,
            MemoryState::Emergency => 0,
        };
        let mut excess = self
            .precharge
            .len()
            .saturating_sub(target)
            .min(TRIM_PER_DROP);
        while excess > 0 {
            let Some(frame) = self.precharge.pop() else {
                break;
            };
            allocprofile::add(&allocprofile::FA_TRIMMED, 1);
            // The pool's own drain path is not a double free: clear the tripwire bit first.
            if frame.clear_pooled() {
                POOLED_FRAMES.fetch_sub(1, Ordering::Relaxed);
            }
            free_frame_nopark(frame);
            excess -= 1;
        }
    }
}

/// Per-cpu, owning cpu only, touched with interrupts disabled, and addressed through
/// [`tls_fa`] so the address is derived from the segment base *at the point of use*.
/// **There is no lock.** Adding any accessor that reaches another cpu's pool -- a reclaim sweep,
/// a stats walk, a debugger -- requires reinstating exclusivity first; draining is to be done by
/// IPI request so that the owning cpu drains its own (faplan.md).
#[thread_local]
static mut TLS_FRAME_ALLOCATOR: Option<FrameAllocator> = None;

/// Offset of [`TLS_FRAME_ALLOCATOR`] from the thread pointer. One ELF TLS template, so one layout,
/// identical on every cpu. Zero means "not computed"; no TLS variable sits at the thread pointer.
static TLS_FA_TPOFF: AtomicUsize = AtomicUsize::new(0);

#[cold]
fn init_tls_fa_tpoff() -> usize {
    // Interrupts off because the two halves -- the variable's address and this cpu's thread
    // pointer -- must come from the *same* cpu, which is the property `tls_fa` exists to keep.
    let int = crate::interrupt::disable();
    let off = (core::ptr::addr_of!(TLS_FRAME_ALLOCATOR) as usize)
        .wrapping_sub(crate::arch::processor::tls_base());
    TLS_FA_TPOFF.store(off, Ordering::Relaxed);
    crate::interrupt::set(int);
    off
}

/// This cpu's pool, addressed so the address cannot be stale.
///
/// The obvious `&mut TLS_FRAME_ALLOCATOR` is **not** safe here, and that is not a theoretical
/// worry: it is the defect tag fadf-audit found in the frame cache this pool replaced. Taking the
/// address of a `#[thread_local]` lets the compiler materialise `thread_pointer + offset` into a
/// general register -- and it hoisted that computation *above* the `cli`, spilled it, and reloaded
/// it inside the critical section. A general register survives migration; the thread pointer does
/// not. So a thread preempted in that window resumes on another cpu and mutates the *previous*
/// cpu's pool while correctly interrupts-off, which is two cpus in one structure.
///
/// Reading the segment base here, under the caller's `with_disabled`, closes the window: plain
/// `asm!` is volatile and cannot be reordered across the interrupt-disable asm, so the base is
/// this cpu's for as long as interrupts stay off. Same reasoning and same shape as
/// `thread::read_current_thread_ptr`, which fixed exactly this for `CURRENT_THREAD`.
///
/// # Safety
/// Caller must hold interrupts disabled for the whole borrow.
#[allow(static_mut_refs)]
#[inline(always)]
unsafe fn tls_fa() -> &'static mut Option<FrameAllocator> {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let mut off = TLS_FA_TPOFF.load(Ordering::Relaxed);
        if core::intrinsics::unlikely(off == 0) {
            off = init_tls_fa_tpoff();
        }
        let base: usize;
        core::arch::asm!(
            "mov {b}, fs:[0]",
            b = lateout(reg) base,
            options(nostack, preserves_flags),
        );
        &mut *(base.wrapping_add(off) as *mut Option<FrameAllocator>)
    }
    // No segment override to lean on. The address is materialised inside the caller's
    // interrupts-off region, which is what `read_current_thread_ptr`'s non-x86 arm settles for
    // too; it carries the same residual risk of the compiler hoisting the computation out.
    #[cfg(not(target_arch = "x86_64"))]
    unsafe {
        &mut *core::ptr::addr_of_mut!(TLS_FRAME_ALLOCATOR)
    }
}

/// Park a freed level-0 frame in this cpu's precharge pool instead of re-entering the PFA.
///
/// The alloc side already batches its PFA traffic (`precharge` fetches in bulk); the free side
/// takes the global lock once per frame, ~866k times a boot. That asymmetry is the whole of what
/// this removes.
///
/// Ownership: per-cpu, owning cpu only, interrupts disabled, addressed through [`tls_fa`] so the
/// address is taken from the segment base at the point of use and cannot be stale. There is no
/// lock -- the global try-lock this used to take was masking the hoisted-address defect (tag
/// fadf-audit) by serialising cpus, at the cost of a contended atomic on every free.
///
/// Never allocates. A free that allocates can recurse into the allocator it is freeing for, so
/// this pushes only into spare capacity and declines to create the pool if there isn't one.
#[allow(static_mut_refs)]
fn park_frame_in_pool(frame: FrameRef) -> bool {
    if framecache::ENABLED || !allocprofile::FA_FREE_TO_POOL {
        return false;
    }
    if !tls_ready() {
        allocprofile::add(&allocprofile::FA_PARK_NO_TLS, 1);
        return false;
    }
    // Pressure is where parking stops: a parked frame is invisible to the PFA, and reclaim
    // relies on frees actually returning memory.
    if memory_state() >= MemoryState::Tight {
        allocprofile::add(&allocprofile::FA_PARK_PRESSURE, 1);
        return false;
    }
    match try_park(frame) {
        ParkResult::Parked => return true,
        ParkResult::Full => {}
        ParkResult::Declined => return false,
    }
    // **The drain has to be reachable from here, not only from `Drop`.** A cpu that just frees --
    // the reaper -- never constructs or drops a `FrameAllocator`, so the `Drop`-side drain never
    // runs on it and its pool sits at the ceiling for the life of the boot, refusing every park.
    // Measured: steals succeeding (`miss` 15,134) while 88% of frees decline `full`, which can
    // only be two different cpus. Draining here costs one burst of up to `TRIM_PER_DROP` frees
    // and buys room for that many subsequent parks.
    if !allocprofile::FA_POOL_WATERMARK {
        allocprofile::add(&allocprofile::FA_PARK_FULL, 1);
        return false;
    }
    drain_pool_to_low_water();
    match try_park(frame) {
        ParkResult::Parked => true,
        ParkResult::Full => {
            allocprofile::add(&allocprofile::FA_PARK_FULL, 1);
            false
        }
        ParkResult::Declined => false,
    }
}

enum ParkResult {
    Parked,
    /// At [`MAX_TLS_PRECHARGE`]. Recoverable by draining.
    Full,
    /// No pool, or no spare vec capacity -- neither of which a drain fixes. Counted at the site.
    Declined,
}

/// One attempt to push `frame` into this cpu's pool. Never allocates: it pushes only into spare
/// capacity and declines to create the pool if there isn't one, because a free that allocates can
/// recurse into the allocator it is freeing for.
fn try_park(frame: FrameRef) -> ParkResult {
    crate::interrupt::with_disabled(|| {
        // Safety: interrupts are disabled for the whole borrow.
        unsafe {
            match *tls_fa() {
                // Deliberately not created here: `FrameAllocator::new` allocates.
                None => {
                    allocprofile::add(&allocprofile::FA_PARK_NO_POOL, 1);
                    ParkResult::Declined
                }
                Some(ref mut fa) => {
                    let v = &mut fa.precharge;
                    if v.len() < MAX_TLS_PRECHARGE && v.len() < v.capacity() {
                        // Parity with `raw_free_frame`, which this path skips. Both were measured
                        // as gaps 2026-08-21: `IS_WIRED` has no other guard on the free path at
                        // all, and the range check is the only live tripwire for the open fa-bulk
                        // corruption -- for which pooling is the leading suspect.
                        assert!(
                            !frame.is_wired(),
                            "parking a wired frame (raw_free_frame would have caught this): {:?}",
                            frame
                        );
                        check_overlap(frame, "park");
                        frame.set_cow(false);
                        assert!(
                            !frame.mark_pooled(),
                            "frame already parked in a pool at free (double free): {:?}",
                            frame
                        );
                        v.push(frame);
                        POOLED_FRAMES.fetch_add(1, Ordering::Relaxed);
                        ParkResult::Parked
                    } else if v.len() >= MAX_TLS_PRECHARGE {
                        ParkResult::Full
                    } else {
                        allocprofile::add(&allocprofile::FA_PARK_NO_CAP, 1);
                        ParkResult::Declined
                    }
                }
            }
        }
    })
}

/// Alloc-side counterpart of [`park_frame_in_pool`]: take level-0 frames back out of this cpu's
/// pool instead of going to the PFA.
///
/// Returns frames **raw** -- still marked POOLED, still charged to whatever class their last
/// owner had, and possibly dirty. Every caller must pass each one through
/// [`finish_parked_alloc`], and must do so *outside* the interrupts-off region, since that can
/// memset 4 KiB.
///
/// Never allocates: it only pops, and the batch form is bounded by `out`'s existing spare
/// capacity.
fn unpark_frame_from_pool() -> Option<FrameRef> {
    if !tls_ready() {
        return None;
    }
    // Safety: interrupts are disabled for the whole borrow.
    let (frame, had_pool) = crate::interrupt::with_disabled(|| unsafe {
        match *tls_fa() {
            None => (None, false),
            Some(ref mut fa) => (fa.precharge.pop(), true),
        }
    });
    if frame.is_some() {
        allocprofile::add(&allocprofile::FA_UNPARKED, 1);
    } else {
        allocprofile::add(&allocprofile::FA_UNPARK_MISS, 1);
        allocprofile::add(
            if had_pool {
                &allocprofile::FA_UNPARK_EMPTY
            } else {
                &allocprofile::FA_UNPARK_NO_POOL
            },
            1,
        );
    }
    frame
}

/// Batch form of [`unpark_frame_from_pool`]. Appends raw frames to `out` and returns how many.
fn unpark_frames_from_pool(want: usize, out: &mut FrameStore) -> usize {
    if want == 0 || !tls_ready() {
        return 0;
    }
    // Spare capacity only, and bounded: this pops with interrupts off, and a `setup_cow_range`
    // precharge asks for ~1030 frames.
    let want = want
        .min(MAX_UNPARK_BATCH)
        .min(out.capacity().saturating_sub(out.len()));
    if want == 0 {
        allocprofile::add(&allocprofile::FA_UNPARK_MISS, 1);
        return 0;
    }
    let before = out.len();
    // Safety: interrupts are disabled for the whole borrow.
    let had_pool = crate::interrupt::with_disabled(|| unsafe {
        if let Some(ref mut fa) = *tls_fa() {
            for _ in 0..want {
                let Some(frame) = fa.precharge.pop() else {
                    break;
                };
                out.push(frame);
            }
            true
        } else {
            false
        }
    });
    let got = out.len() - before;
    if got == 0 {
        allocprofile::add(&allocprofile::FA_UNPARK_MISS, 1);
        allocprofile::add(
            if had_pool {
                &allocprofile::FA_UNPARK_EMPTY
            } else {
                &allocprofile::FA_UNPARK_NO_POOL
            },
            1,
        );
    } else {
        allocprofile::add(&allocprofile::FA_UNPARKED, got as u64);
    }
    got
}

/// How many more frames this cpu's pool can accept, using the same bound `merge` uses so that a
/// surplus sized by this is guaranteed to fit rather than becoming `leftover`.
fn pool_headroom() -> usize {
    if !tls_ready() {
        return 0;
    }
    // Safety: interrupts are disabled for the whole borrow.
    crate::interrupt::with_disabled(|| unsafe {
        match *tls_fa() {
            None => 0,
            Some(ref fa) => fa
                .precharge
                .capacity()
                .saturating_sub(MAX_FA_FRAMES)
                .min(MAX_TLS_PRECHARGE)
                .saturating_sub(fa.precharge.len()),
        }
    })
}

/// Drain this cpu's pool to [`POOL_LOW_WATER`] once it reaches [`POOL_HIGH_WATER`].
///
/// **This is what `trim` can no longer do.** Under [`allocprofile::FA_NO_TAKE`] a dropping
/// allocator is the small surplus, not the pool, so `trim` never sees the pool at all -- and even
/// before that it was inert, because `MemoryState::Plenty` targets `MAX_TLS_PRECHARGE`, which is
/// the ceiling.
///
/// Frames are popped inside the interrupts-off region and freed **outside** it: `free_frame_nopark`
/// takes the PFA lock, and holding that with interrupts disabled for up to `TRIM_PER_DROP` frames
/// is exactly the kind of long critical section the zeroing was moved out of the allocator lock to
/// avoid. Bounded per call for the same reason `trim` is; drops are frequent enough that the pool
/// converges in a few of them.
fn drain_pool_to_low_water() {
    if !allocprofile::FA_POOL_WATERMARK || !tls_ready() {
        return;
    }
    let mut buf: heapless::Vec<FrameRef, TRIM_PER_DROP> = heapless::Vec::new();
    // Safety: interrupts are disabled for the whole borrow, and nothing here allocates.
    crate::interrupt::with_disabled(|| unsafe {
        if let Some(ref mut fa) = *tls_fa() {
            if fa.precharge.len() < POOL_HIGH_WATER {
                return;
            }
            while fa.precharge.len() > POOL_LOW_WATER && !buf.is_full() {
                let Some(frame) = fa.precharge.pop() else {
                    break;
                };
                if buf.push(frame).is_err() {
                    // Cannot drop it on the floor: put it back.
                    fa.precharge.push(frame);
                    break;
                }
            }
        }
    });
    for frame in buf {
        if frame.clear_pooled() {
            POOLED_FRAMES.fetch_sub(1, Ordering::Relaxed);
        }
        allocprofile::add(&allocprofile::FA_TRIMMED, 1);
        free_frame_nopark(frame);
    }
}

/// Always succeeds now. It used to be able to fail -- another cpu holding the single global flag
/// -- and the failure path in `FrameAllocator::drop` *freed* the whole pool instead of saving it
/// (`save locked=3197` a boot). That was never protecting this cpu's pool from this cpu; it was
/// serialising cpus to mask a stale-address bug that is fixed properly by [`tls_fa`].
/// Give this cpu's pool its buffer, once, from a context that is allowed to allocate.
///
/// Everything that *uses* the pool is forbidden to allocate: `try_park` runs on the free path and
/// a free that allocates can recurse into the allocator it is freeing for, and `merge` runs from
/// `FrameAllocator::drop`, which can be one `GlobalPageAlloc::extend` built while `allocate_chunk`
/// holds `GLOBAL_PAGE_ALLOC`. So the pool's capacity has to arrive from somewhere else.
///
/// Until now it arrived by accident: `precharge` reserved `MAX_TLS_PRECHARGE + MAX_FA_FRAMES` on
/// *whatever allocator was passing through*, and `merge`'s wholesale swap handed that buffer to
/// the pool. That made the pool's depth a side effect of a hot-path reserve, which is why it
/// collapsed three separate times today the moment a caller stopped precharging pool-sized.
///
/// The allocation happens outside the interrupts-off region and the install inside it, so the
/// ownership rule (`tls_fa` is touched only by its owning cpu, only with interrupts disabled)
/// holds throughout. Idempotent, and a no-op after the first call on each cpu.
fn ensure_pool_provisioned() {
    if !tls_ready() {
        return;
    }
    // Safety: interrupts are disabled for the whole borrow.
    let short = crate::interrupt::with_disabled(|| unsafe {
        match *tls_fa() {
            Some(ref fa) => fa.precharge.capacity() < POOL_VEC_CAPACITY,
            None => true,
        }
    });
    if !short {
        return;
    }
    // Outside the critical section: this is the allocation the pool paths may not make.
    let mut buf = FrameStore::with_heap_capacity(POOL_VEC_CAPACITY);
    let mut installed = false;
    // Safety: interrupts are disabled for the whole borrow, and nothing below allocates --
    // `buf` already has the capacity every push here needs.
    crate::interrupt::with_disabled(|| unsafe {
        let slot = tls_fa();
        if slot.is_none() {
            let mut pool = FrameAllocator::new(
                FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
                PHYS_LEVEL_LAYOUTS[0],
            );
            pool.avoid_alloc = true;
            *slot = Some(pool);
        }
        let pool = slot.as_mut().unwrap();
        if pool.precharge.capacity() >= POOL_VEC_CAPACITY {
            // Another path provisioned it between the two regions above.
            return;
        }
        // `POOL_VEC_CAPACITY >= MAX_TLS_PRECHARGE`, and `try_park` refuses past that, so every
        // frame currently pooled fits in `buf` by construction.
        // `len < capacity` checked explicitly rather than relying on `push` not to grow: `push`
        // on a full vec allocates, and this runs with interrupts disabled inside the allocator.
        // `POOL_VEC_CAPACITY >= MAX_TLS_PRECHARGE`, and `try_park` refuses past that, so the
        // branch below is unreachable in practice -- it is here so that "cannot allocate" is a
        // property of the code rather than of an argument about the pool's depth.
        while let Some(frame) = pool.precharge.pop() {
            if buf.len() == buf.capacity() {
                // Cannot drop a frame on the floor; put it back and leave the pool as it was.
                pool.precharge.push(frame);
                return;
            }
            buf.push(frame);
        }
        core::mem::swap(&mut pool.precharge, &mut buf);
        allocprofile::add(&allocprofile::FA_POOL_PROVISIONED, 1);
        installed = true;
    });
    // Logged, not only counted. This fires once per cpu during boot, and `perfmark` prints every
    // allocprofile counter as a *delta between two marks* -- so `FA_POOL_PROVISIONED` reads 0 in
    // every bench window whether or not it ever happened, which is a detector that cannot observe
    // its own event. The counter is kept for a boot-wide dump; the line is what makes the log
    // answer the question.
    if installed {
        logln!(
            "allocprofile: pool provisioned, {} slots",
            POOL_VEC_CAPACITY
        );
    }
    // `buf` is now the old, empty buffer. Freed here, outside the region.
    drop(buf);
}

pub fn save_frame_allocator(fa: &mut FrameAllocator) -> bool {
    crate::interrupt::with_disabled(|| {
        // Safety: interrupts are disabled for the whole borrow.
        unsafe {
            let slot = tls_fa();
            if let Some(ref mut pool) = *slot {
                pool.merge(fa);
            } else {
                let mut pool = FrameAllocator::new(
                    FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
                    PHYS_LEVEL_LAYOUTS[0],
                );
                pool.merge(fa);
                pool.avoid_alloc = true;
                *slot = Some(pool);
            }
        }
        true
    })
}

/// This cpu's pool depth. Deliberately cpu-local: a system-wide figure must come from the
/// [`POOLED_FRAMES`] gauge, not from walking other cpus' pools, which is the access this design
/// does not permit.
pub fn count_precharged_frames() -> usize {
    if !tls_ready() {
        return 0;
    }
    // Safety: interrupts are disabled for the whole borrow.
    crate::interrupt::with_disabled(|| unsafe {
        tls_fa().as_ref().map(|fa| fa.precharge.len()).unwrap_or(0)
    })
}

#[allow(static_mut_refs)]
pub fn take_frame_allocator() -> Option<FrameAllocator> {
    if !tls_ready() {
        return None;
    }
    if current_thread_ref().is_some_and(|ct| ct.is_critical()) {
        log::warn!("warning -- cannot take frame allocator while in critical section");
        return None;
    }
    // Safety: interrupts are disabled for the whole borrow. Cannot fail for lock reasons any
    // more: the arm that used to lose a race with another cpu -- and send this caller off to
    // build a fresh empty allocator while its own pool sat untouched, `take locked=876` a boot --
    // is gone with the lock.
    crate::interrupt::with_disabled(|| unsafe {
        let fa = tls_fa().take();
        if fa.is_none() {
            allocprofile::add(&allocprofile::FA_TAKE_NONE, 1);
        }
        fa
    })
}

/// An allocator for one operation.
///
/// Under [`allocprofile::FA_NO_TAKE`] this **never empties the TLS slot**: the caller gets a
/// fresh, empty allocator and steals frames from the pool through the drain in
/// [`MemoryTracker::try_alloc_frame`]/`try_alloc_frames` as it precharges. The pool therefore
/// stays reachable to every other allocation and every free on this cpu for the whole operation,
/// which is the property the take destroyed -- 99.9995% of the fault path's drain misses were
/// `no-pool`, not `empty`.
///
/// It also tightens the ownership rules rather than loosening them: with nothing ever holding the
/// pool by value, the only accesses left are the short interrupts-off borrows in
/// `park_frame_in_pool`, `unpark_*` and `save_frame_allocator`, none of which allocate.
pub fn take_or_new_frame_allocator() -> FrameAllocator {
    if allocprofile::FA_NO_TAKE {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
            PHYS_LEVEL_LAYOUTS[0],
        );
        fa.avoid_alloc = true;
        return fa;
    }
    take_frame_allocator().unwrap_or_else(|| {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
            PHYS_LEVEL_LAYOUTS[0],
        );
        fa.avoid_alloc = true;
        fa
    })
}

impl Drop for FrameAllocator {
    fn drop(&mut self) {
        allocprofile::add(
            &allocprofile::FA_DROP_FRAMES,
            (self.precharge.len() + self.abort.len()) as u64,
        );
        // Note that the abort list is recycled by the save/take path below rather than freed:
        // abort frames can carry a non-zero refcount (a failed map after an rc bump), which
        // `free_frame` refuses outright. Parking feeds only from `free_frame` (rc==0 asserted).
        if framecache::ENABLED && tls_ready() && self.layout == PHYS_LEVEL_LAYOUTS[0] {
            // Surplus goes back to the cache, not into a per-cpu pool this allocator owns. That is
            // the whole of R2: an operation borrows and returns, and between operations the frames
            // live somewhere every other draw and every other free on this cpu can reach.
            //
            // Only the precharge list. The abort list keeps its existing path through `clear()`
            // below, untouched: abort frames are already marked `POOLED` by `abort()`, so offering
            // one here would trip the cache's own double-mark assert, and some carry a non-zero
            // refcount, which is a pre-existing gap documented at `clear` and not this change's to
            // close.
            // Clean, not dirty. These were never popped by `try_allocate`, so nobody wrote them;
            // returning them as dirty makes the cache memset a frame it is about to hand straight
            // back, which is 99.98% of hand-outs on `object_map_unmap_syscall`.
            let known_zero = PRECHARGE_RETURNS_CLEAN && self.precharge_known_zero;
            let mut given = 0u64;
            while let Some(frame) = self.precharge.pop() {
                if cache_freed_frame_hinted(frame, known_zero) {
                    given += 1;
                } else {
                    free_frame_nopark(frame);
                }
            }
            allocprofile::add(&allocprofile::FA_DROP_SAVED, given.min(1));
            allocprofile::add(&allocprofile::FA_TRIMMED, given);
            let t = allocprofile::start();
            self.clear();
            allocprofile::record(&allocprofile::FA_DROP_CLEAR_NS, t);
        } else if tls_ready() && self.layout == PHYS_LEVEL_LAYOUTS[0] {
            self.trim();
            let t = allocprofile::start();
            let saved = save_frame_allocator(self);
            allocprofile::record(&allocprofile::FA_DROP_SAVE_NS, t);
            // After the save, so the level this reads includes what we just handed back.
            drain_pool_to_low_water();
            if !saved {
                allocprofile::add(&allocprofile::FA_DROP_CLEARED, 1);
            } else {
                allocprofile::add(&allocprofile::FA_DROP_SAVED, 1);
            }
            // Unconditional now: a bounded `merge` can decline frames that did not fit, and a
            // `Vec` field drop would leak them (`FrameRef` is a borrowed reference, not an owner).
            // A no-op when the save took everything, which is the common case.
            let t = allocprofile::start();
            self.clear();
            allocprofile::record(&allocprofile::FA_DROP_CLEAR_NS, t);
        } else {
            allocprofile::add(&allocprofile::FA_DROP_CLEARED, 1);
            let t = allocprofile::start();
            self.clear();
            allocprofile::record(&allocprofile::FA_DROP_CLEAR_NS, t);
        }
    }
}

pub struct FrameRegion {
    pub range: PhysRange,
    pub flags: FrameAllocFlags,
}

pub struct FrameIter {
    range: PhysRange,
    n: usize,
}

impl FrameIter {
    pub fn new(range: PhysRange) -> Self {
        Self { range, n: 0 }
    }
}

impl Iterator for FrameIter {
    type Item = FrameRef;

    fn next(&mut self) -> Option<Self::Item> {
        let n = self.n;
        self.n += 1;
        let page = self.range.pages().nth(n)?;
        get_frame(PhysAddr::new(page).ok()?)
    }
}

impl FrameRegion {
    pub fn frames(&self) -> FrameIter {
        FrameIter::new(self.range)
    }

    pub fn num_frames(&self) -> usize {
        self.range.len() / FRAME_SIZE
    }
}
