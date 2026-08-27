//! This mod implements [UserContext] and [KernelMemoryContext] for virtual memory systems.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    marker::PhantomData,
    mem::size_of,
    ops::Range,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use intrusive_collections::{KeyAdapter, RBTree, RBTreeAtomicLink, intrusive_adapter};
use region::{MapRegion, RegionManager};
use twizzler_abi::{
    device::CacheType,
    object::{MAX_SIZE, NULLPAGE_SIZE, ObjID, Protections},
    syscall::{MapControlCmd, MapFlags},
};
use twizzler_rt_abi::error::{ResourceError, TwzError};

use super::{
    KernelMemoryContext, KernelObjectHandle, ObjectContextInfo, UserContext, kernel_context,
};
use crate::{
    arch::{
        address::VirtAddr,
        context::{ArchContext, ArchContextTarget},
    },
    idcounter::{Id, IdCounter, StableId},
    memory::{
        PhysAddr,
        frame::{FrameRef, PHYS_LEVEL_LAYOUTS},
        pagetables::{
            ContiguousProvider, Mapper, MappingCursor, MappingFlags, MappingSettings,
            PhysAddrProvider, PhysMapInfo, Table, UninitPageProvider,
        },
        tracker::{FrameAllocFlags, FrameAllocator, take_or_new_frame_allocator},
    },
    mutex::Mutex,
    obj::{
        LookupFlags, ObjectRef, PageNumber, PtGuard, lookup_object, pagetables::ObjectPageTable,
    },
    once::Once,
    processor::{
        mp::current_processor,
        sched::{SchedFlags, schedule},
        spin_wait_until, tls_ready,
    },
    security::KERNEL_SCTX,
    spinlock::Spinlock,
    thread::current_thread_ref,
};

pub mod fault;
pub mod region;
pub mod regionmgr;
mod tests;

pub use fault::page_fault;

/// A type that implements [super::Context] for virtual memory systems.
pub struct VirtContext {
    secctx: Mutex<RBTree<SecctxAdapter>>,
    /// The kernel context's arch state, held outside `secctx` because the kernel has exactly one
    /// security context: there is no map to consult and no lock to take. `Some` here is what makes
    /// this the kernel context.
    ///
    /// Not merely an optimization. Kernel heap growth reaches [`VirtContext::with_arch`] from
    /// inside the allocator's critical section -- ferroc's base allocator calls `allocate_chunk`,
    /// which calls [`GlobalPageAlloc::extend`] -- and `secctx` is a *sleeping* mutex, so taking it
    /// there is the `cannot lock mutex in critical context` panic that `stabilitybugs.md` calls
    /// Mode C.
    kernel_arch: Option<ArchContext>,
    // We keep a cache of the actual switch targets so that we don't need to take the above mutex
    // during switch_to. Unfortunately, it's still kinda hairy, since this is a spinlock of a
    // memory-allocating collection. See register_sctx for details.
    target_cache: Spinlock<RBTree<TargetAdapter>>,
    regions: RegionManager,
    /// Identity for [`SlotMemo`] entries, from a counter that never reuses.
    ///
    /// Deliberately *not* `id`: `IdCounter::next` pops from a reuse pool, so a dropped context's
    /// id is handed to a later one. Under the generation scheme that could not bite -- any mapping
    /// change swept every memo, so no entry survived long enough to meet a recycled id -- but
    /// per-region validation removes exactly that sweep, and a stale entry matching a recycled id
    /// would pass a liveness check on a region its context no longer binds.
    memo_tag: u64,
    id: Id<'static>,
    is_kernel: bool,
}

/// The kernel context's page-table root, cached at boot so that the thread-switch path can reach
/// it without taking any lock. See [`VirtContext::switch_to_kernel_context`].
static KERNEL_ARCH_TARGET: Once<ArchContextTarget> = Once::new();

/// `allocate_chunk` traffic, printed at debug shutdown next to the other kernel profiles.
///
/// What this is for: every kernel heap allocation ferroc cannot satisfy from memory it already
/// holds lands in [`KernelMemoryContext::allocate_chunk`] and takes `GLOBAL_PAGE_ALLOC`, one
/// spinlock for the whole machine -- and on the growth path it holds that lock across a frame
/// allocation per page plus a full `arch.map`, TLB shootdown included. Whether that matters
/// depends entirely on the call rate, which nothing measured: ferroc's slabs may absorb
/// essentially all of it, in which case the lock is uncontended and the growth path is a boot-time
/// cost, or they may not.
///
/// Counts are unconditional -- a relaxed increment on a path that already takes a global spinlock
/// is nothing -- but only growth is timed, since it is rare and already expensive enough that two
/// clock reads are noise. Deliberately no timing on the fast path: that is the hot one, and
/// `TIMING_ON`-style gating would answer a question (`how long is the lock held`) that the grow
/// count plus the fast/slow ratio already answers well enough to decide whether to look further.
pub mod heapprofile {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::instant::Instant;

    static CALLS: AtomicU64 = AtomicU64::new(0);
    static BYTES: AtomicU64 = AtomicU64::new(0);
    static FREES: AtomicU64 = AtomicU64::new(0);
    static GROWS: AtomicU64 = AtomicU64::new(0);
    static GROW_BYTES: AtomicU64 = AtomicU64::new(0);
    static GROW_NS: AtomicU64 = AtomicU64::new(0);

    pub fn record_alloc(size: usize) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size as u64, Ordering::Relaxed);
    }

    pub fn record_free() {
        FREES.fetch_add(1, Ordering::Relaxed);
    }

    /// Charged for a growth, which is the arm that maps and shoots down.
    pub fn record_grow(bytes: usize, start: Instant) {
        GROWS.fetch_add(1, Ordering::Relaxed);
        GROW_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
        GROW_NS.fetch_add(
            (Instant::now() - start).as_nanos() as u64,
            Ordering::Relaxed,
        );
    }

    pub fn print() {
        let calls = CALLS.load(Ordering::Relaxed);
        if calls == 0 {
            return;
        }
        let bytes = BYTES.load(Ordering::Relaxed);
        let frees = FREES.load(Ordering::Relaxed);
        let grows = GROWS.load(Ordering::Relaxed);
        let grow_bytes = GROW_BYTES.load(Ordering::Relaxed);
        let grow_ns = GROW_NS.load(Ordering::Relaxed);
        logln!(
            "== allocate_chunk: {} calls ({} KB, {} B each), {} frees; {} grows ({} KB, {} us total, {} us each), 1 grow per {} calls ==",
            calls,
            bytes / 1024,
            bytes / calls,
            frees,
            grows,
            grow_bytes / 1024,
            grow_ns / 1000,
            if grows == 0 {
                0
            } else {
                grow_ns / grows / 1000
            },
            if grows == 0 { 0 } else { calls / grows },
        );
    }
}

/// `insert_object` split, printed at debug shutdown next to the other kernel profiles.
///
/// The monitor's `SPACESTAT` put ~110 us per cold map inside this syscall and ~350 ns in the
/// monitor itself, so this is where that time has to be. `check_id` is the first suspect because
/// it reads the object's meta page, which for a pager-backed object can be a round trip.
pub mod mapprofile {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::instant::Instant;

    /// Whether the map path keeps the whole-call and `check_id` timings it has always kept.
    ///
    /// These were unconditional: `Timer` plus `record_checkid` here, and four more clock reads in
    /// `sys_object_map`'s `mapstats`. Every one ends in an `as_nanos()`, which is a u128 multiply
    /// and two u128 divisions (see [`crate::instant::Instant`]'s own comment on why it does not
    /// convert eagerly) -- roughly seven clock reads and five conversions per map syscall, on the
    /// path `object_map_unmap_syscall`, `file_open` and `object_create_delete` all measure. That
    /// is F11's shape exactly, so it is now off by default and this const is the A/B switch back.
    pub const MAP_STATS: bool = false;

    /// Whether `insert_object` additionally splits itself by stage. Purely an attribution
    /// instrument, added after `MAP_STATS`; separate from it so a baseline arm can restore the
    /// old always-on timings without also being charged for this.
    pub const MAP_PROFILE: bool = false;

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static CHECKID: AtomicU64 = AtomicU64::new(0);
    static TOTAL: AtomicU64 = AtomicU64::new(0);

    /// Stages of `insert_object`, which is ~7.8 us of the ~8.3 us `sys_object_map` costs.
    #[derive(Clone, Copy)]
    #[repr(usize)]
    pub enum Stage {
        /// `cow_clone_page_tables`, for a STABLE mapping only.
        Stable = 0,
        /// Building the `MapRegion`: an object `Arc` clone and two fresh `Arc<AtomicBool>`s.
        Region,
        /// `take_or_new_frame_allocator` + `precharge_slot_map`, ahead of the lock.
        Precharge,
        /// Acquiring the context-wide `regions` mutex -- F9's convoy.
        Lock,
        /// `map_object`: the arch mapper walk plus its TLB consistency.
        MapObj,
        /// `insert_region` into the interval tree.
        Insert,
        Total,
    }

    pub const NR: usize = Stage::Total as usize + 1;
    pub const NAMES: [&str; NR] = [
        "stable",
        "region",
        "precharge",
        "lock",
        "map_obj",
        "insert",
        "TOTAL",
    ];

    static STAGE_COUNT: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];
    static STAGE_NS: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];

    #[inline(always)]
    pub fn start() -> Instant {
        if MAP_PROFILE {
            Instant::now()
        } else {
            Instant::zero()
        }
    }

    /// The clock read behind [`MAP_STATS`], as opposed to [`start`]'s behind [`MAP_PROFILE`].
    #[inline(always)]
    pub fn stats_stamp() -> Instant {
        if MAP_STATS {
            Instant::now()
        } else {
            Instant::zero()
        }
    }

    pub fn record(stage: Stage, start: Instant) {
        if !MAP_PROFILE {
            return;
        }
        let ns = (Instant::now() - start).as_nanos() as u64;
        STAGE_COUNT[stage as usize].fetch_add(1, Ordering::Relaxed);
        STAGE_NS[stage as usize].fetch_add(ns, Ordering::Relaxed);
    }

    /// Per-stage (count, nanoseconds), cumulative, for [`crate::perfmark`] to difference.
    pub fn snapshot() -> [(u64, u64); NR] {
        let mut out = [(0u64, 0u64); NR];
        if !MAP_PROFILE {
            return out;
        }
        for i in 0..NR {
            out[i] = (
                STAGE_COUNT[i].load(Ordering::Relaxed),
                STAGE_NS[i].load(Ordering::Relaxed),
            );
        }
        out
    }

    pub fn record_checkid(ns: u64) {
        if !MAP_STATS {
            return;
        }
        CHECKID.fetch_add(ns, Ordering::Relaxed);
    }

    /// Charges the whole of `insert_object` on drop, so an early return is counted too.
    pub struct Timer(pub Instant);

    impl Drop for Timer {
        fn drop(&mut self) {
            if !MAP_STATS {
                return;
            }
            COUNT.fetch_add(1, Ordering::Relaxed);
            TOTAL.fetch_add(
                (Instant::now() - self.0).as_nanos() as u64,
                Ordering::Relaxed,
            );
        }
    }

    pub fn print() {
        let n = COUNT.load(Ordering::Relaxed);
        if n > 0 {
            let total = TOTAL.load(Ordering::Relaxed);
            let check = CHECKID.load(Ordering::Relaxed);
            logln!(
                "== insert_object: {} calls, {} us total; per call {} ns = check_id {} + rest {} ==",
                n,
                total / 1000,
                total / n,
                check / n,
                total.saturating_sub(check) / n,
            );
        }
        for (i, name) in NAMES.iter().enumerate() {
            let c = STAGE_COUNT[i].load(Ordering::Relaxed);
            if c == 0 {
                continue;
            }
            logln!(
                "  {:>9}: {} calls, {} ns/call",
                name,
                c,
                STAGE_NS[i].load(Ordering::Relaxed) / c
            );
        }
    }
}

/// Stage split of [`VirtContext::map_object`] — the inside of `insert_object`'s `map_obj` stage
/// (948 ns/call in mapsplit1, the largest map-side item, previously opaque; `MAP_PROBE` covers
/// only the fault path's `map_page`, not this). Same pattern as [`mapprofile`]; gated, ships OFF.
pub mod mapobjprofile {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::instant::Instant;

    pub const MAPOBJ_PROFILE: bool = false;

    #[derive(Clone, Copy)]
    #[repr(usize)]
    pub enum Stage {
        /// `security::get_sctx`.
        Sctx = 0,
        /// `sctx.lookup` — the per-map capability/permission lookup.
        Lookup,
        /// Taking the object's page-table sleeping mutex (or the stable clone's).
        PtLock,
        /// `try_with_arch`, whole, including the closure below.
        Arch,
        /// Within [Stage::Arch]: `pt.add_invalidate`.
        AddInv,
        /// Within [Stage::Arch]: `arch.object_map` plus the map-count charge.
        ObjMap,
        Total,
    }

    pub const NR: usize = Stage::Total as usize + 1;
    pub const NAMES: [&str; NR] = [
        "sctx", "lookup", "pt_lock", "arch", "add_inv", "obj_map", "TOTAL",
    ];

    static STAGE_COUNT: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];
    static STAGE_NS: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];

    #[inline(always)]
    pub fn start() -> Instant {
        if MAPOBJ_PROFILE {
            Instant::now()
        } else {
            Instant::zero()
        }
    }

    pub fn record(stage: Stage, start: Instant) {
        if !MAPOBJ_PROFILE {
            return;
        }
        let ns = (Instant::now() - start).as_nanos() as u64;
        STAGE_COUNT[stage as usize].fetch_add(1, Ordering::Relaxed);
        STAGE_NS[stage as usize].fetch_add(ns, Ordering::Relaxed);
    }

    /// Per-stage (count, nanoseconds), cumulative, for [`crate::perfmark`] to difference.
    pub fn snapshot() -> [(u64, u64); NR] {
        let mut out = [(0u64, 0u64); NR];
        if !MAPOBJ_PROFILE {
            return out;
        }
        for i in 0..NR {
            out[i] = (
                STAGE_COUNT[i].load(Ordering::Relaxed),
                STAGE_NS[i].load(Ordering::Relaxed),
            );
        }
        out
    }
}

/// Stage split of [`VirtContext::remove_object`], the whole of `sys_object_unmap`.
///
/// Separate from [`mapprofile`] rather than folded into it: the two paths have nothing in common
/// past the slot number, and an unmap costs its own precharge, its own page-table lock and its own
/// shootdown wait. `object_create_delete` pays both once per iteration and nothing had ever split
/// the second one.
pub mod unmapprofile {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::instant::Instant;

    pub const UNMAP_PROFILE: bool = false;

    #[derive(Clone, Copy)]
    #[repr(usize)]
    pub enum Stage {
        /// `FrameAllocator::new` (a precharge) plus `begin_remove`.
        Pre = 0,
        /// `remove_mapping` + `note_unmap`.
        Notify,
        /// Acquiring the object's page-table lock -- a sleeping mutex.
        Lock,
        /// The `for_each_arch` unmap loop: mapper walks and `remove_invalidate`.
        Arches,
        /// Dropping the page-table guard, i.e. the shootdown wait and the deferred frame frees.
        Shoot,
        /// `guard.finish`, the sync check, and the reap request.
        Finish,
        /// Within [Stage::Arches]: `pt.members()`, the membership filter.
        Members,
        /// Within [Stage::Arches]: `ArchContext::unmap_object`.
        UnmapObj,
        /// Within [Stage::Arches]: `pt.remove_invalidate` and the map-count bookkeeping.
        RemInv,
        /// Within [Stage::UnmapObj]: taking the arch mapper's spinlock.
        UoLock,
        /// Within [Stage::UnmapObj]: the page-table walk itself.
        UoWalk,
        /// Within [Stage::UnmapObj]: `Consistency::finish_send` -- IPI distribution, no wait.
        UoSend,
        /// Within [Stage::UnmapObj]: `run_all` -- the shootdown wait plus the frame frees.
        UoRun,
        /// Within [Stage::Finish]: `RemoveGuard::finish`, i.e. the slot state swap.
        FinSwap,
        /// Within [Stage::Finish]: `request_reap`, i.e. the reaper queue push and wake.
        FinReap,
        /// Within [Stage::FinReap]: taking the reaper's queue lock and pushing.
        ReapPush,
        /// Within [Stage::FinReap]: `CondVar::signal`, i.e. waking the reaper thread.
        ReapSignal,
        /// Within `ArchTlbMgr::finish_send`: the PCID revocation walk. Recorded from every caller,
        /// not just the unmap path -- the split is of the shootdown, which the map path shares.
        SendRevoke,
        /// Within `finish_send`: target selection plus the shootdown statistics.
        SendTarget,
        /// Within `finish_send`: the IPI itself.
        SendIpi,
        /// Within `finish_send`: this processor's own invalidation.
        SendLocal,
        Total,
    }

    pub const NR: usize = Stage::Total as usize + 1;
    pub const NAMES: [&str; NR] = [
        "pre",
        "notify",
        "lock",
        "arches",
        "shoot",
        "finish",
        "members",
        "unmap_obj",
        "rem_invl",
        "uo_lock",
        "uo_walk",
        "uo_send",
        "uo_run",
        "fin_swap",
        "fin_reap",
        "reap_push",
        "reap_signal",
        "snd_revoke",
        "snd_target",
        "snd_ipi",
        "snd_local",
        "TOTAL",
    ];

    static STAGE_COUNT: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];
    static STAGE_NS: [AtomicU64; NR] = [const { AtomicU64::new(0) }; NR];

    #[inline(always)]
    pub fn start() -> Instant {
        if UNMAP_PROFILE {
            Instant::now()
        } else {
            Instant::zero()
        }
    }

    pub fn record(stage: Stage, start: Instant) {
        if !UNMAP_PROFILE {
            return;
        }
        let ns = (Instant::now() - start).as_nanos() as u64;
        STAGE_COUNT[stage as usize].fetch_add(1, Ordering::Relaxed);
        STAGE_NS[stage as usize].fetch_add(ns, Ordering::Relaxed);
    }

    /// Per-stage (count, nanoseconds), cumulative, for [`crate::perfmark`] to difference.
    pub fn snapshot() -> [(u64, u64); NR] {
        let mut out = [(0u64, 0u64); NR];
        if !UNMAP_PROFILE {
            return out;
        }
        for i in 0..NR {
            out[i] = (
                STAGE_COUNT[i].load(Ordering::Relaxed),
                STAGE_NS[i].load(Ordering::Relaxed),
            );
        }
        out
    }

    pub fn print() {
        if !UNMAP_PROFILE {
            return;
        }
        let total = STAGE_COUNT[Stage::Total as usize].load(Ordering::Relaxed);
        if total == 0 {
            return;
        }
        logln!("== remove_object profile: {} calls ==", total);
        for (i, name) in NAMES.iter().enumerate() {
            let c = STAGE_COUNT[i].load(Ordering::Relaxed);
            if c == 0 {
                continue;
            }
            logln!(
                "  {:>9}: {} calls, {} ns/call",
                name,
                c,
                STAGE_NS[i].load(Ordering::Relaxed) / c
            );
        }
    }

    /// Per-call distribution of `remove_object`, split by who initiated the removal. A mean
    /// cannot tell uniform inflation from tail spikes (spawnbench.md §41a wants exactly that
    /// distinction for spawn-phase unmaps), so this keeps a log2 histogram and a per-window
    /// maximum instead. Separate const from [`UNMAP_PROFILE`]: two clock reads per removal when
    /// on, nothing when off.
    pub const UNMAP_HIST: bool = false;

    /// Who asked for this removal. `Own`/`Handle` are the two `sys_object_unmap` forms (a thread
    /// unmapping its own context vs. operating on another context by handle — the monitor's
    /// deferred unmapper is the main `Handle` caller). `Sweep` named the sctx-unregister region
    /// sweep, which is gone -- the monitor's refcounted `MapHandle` teardown releases those
    /// mappings now. Kept so the histogram's slot numbering stays comparable with older runs.
    #[derive(Clone, Copy)]
    #[repr(usize)]
    pub enum Initiator {
        Own = 0,
        Handle,
        Sweep,
    }
    pub const NR_INIT: usize = 3;
    pub const INIT_NAMES: [&str; NR_INIT] = ["own", "handle", "sweep"];
    /// Bucket upper bounds in ns; the last bucket is everything at or above the final bound.
    const HIST_BOUNDS_NS: [u64; 7] = [1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 64_000];
    pub const NR_HBUCKETS: usize = HIST_BOUNDS_NS.len() + 1;

    static H_COUNT: [AtomicU64; NR_INIT] = [const { AtomicU64::new(0) }; NR_INIT];
    static H_NS: [AtomicU64; NR_INIT] = [const { AtomicU64::new(0) }; NR_INIT];
    static H_MAX: [AtomicU64; NR_INIT] = [const { AtomicU64::new(0) }; NR_INIT];
    static HIST: [[AtomicU64; NR_HBUCKETS]; NR_INIT] =
        [const { [const { AtomicU64::new(0) }; NR_HBUCKETS] }; NR_INIT];

    #[inline(always)]
    pub fn hist_stamp() -> Instant {
        if UNMAP_HIST {
            Instant::now()
        } else {
            Instant::zero()
        }
    }

    pub fn record_hist(init: Initiator, start: Instant) {
        if !UNMAP_HIST {
            return;
        }
        let ns = (Instant::now() - start).as_nanos() as u64;
        let i = init as usize;
        H_COUNT[i].fetch_add(1, Ordering::Relaxed);
        H_NS[i].fetch_add(ns, Ordering::Relaxed);
        H_MAX[i].fetch_max(ns, Ordering::Relaxed);
        let b = HIST_BOUNDS_NS
            .iter()
            .position(|bound| ns < *bound)
            .unwrap_or(NR_HBUCKETS - 1);
        HIST[i][b].fetch_add(1, Ordering::Relaxed);
    }

    /// Flat cumulative snapshot for [`crate::perfmark`] to difference: per initiator, (count, ns)
    /// then the buckets.
    pub const NR_HSNAP: usize = NR_INIT * (2 + NR_HBUCKETS);
    pub fn hist_snapshot() -> [u64; NR_HSNAP] {
        let mut out = [0u64; NR_HSNAP];
        if !UNMAP_HIST {
            return out;
        }
        for i in 0..NR_INIT {
            let base = i * (2 + NR_HBUCKETS);
            out[base] = H_COUNT[i].load(Ordering::Relaxed);
            out[base + 1] = H_NS[i].load(Ordering::Relaxed);
            for b in 0..NR_HBUCKETS {
                out[base + 2 + b] = HIST[i][b].load(Ordering::Relaxed);
            }
        }
        out
    }

    /// Maximum per initiator since the last call, reset on read. Not differenceable like the
    /// counters, so the window semantics live here instead of in the caller's `prev` snapshot.
    pub fn take_hist_max() -> [u64; NR_INIT] {
        let mut out = [0u64; NR_INIT];
        if !UNMAP_HIST {
            return out;
        }
        for i in 0..NR_INIT {
            out[i] = H_MAX[i].swap(0, Ordering::Relaxed);
        }
        out
    }
}

static CONTEXT_IDS: IdCounter = IdCounter::new();

struct KernelSlotCounter {
    cur_kernel_slot: usize,
    kernel_slots_nums: Vec<Slot>,
}

static KERNEL_SLOT_COUNTER: Once<Mutex<KernelSlotCounter>> = Once::new();

fn kernel_slot_counter() -> &'static Mutex<KernelSlotCounter> {
    KERNEL_SLOT_COUNTER.call_once(|| {
        Mutex::new(KernelSlotCounter {
            cur_kernel_slot: Slot::try_from(VirtAddr::start_kernel_object_memory())
                .unwrap()
                .raw(),
            kernel_slots_nums: Vec::new(),
        })
    })
}

/// A representation of a slot number.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct Slot(usize);

impl Slot {
    fn start_vaddr(&self) -> VirtAddr {
        VirtAddr::new((self.0 * MAX_SIZE) as u64).unwrap()
    }

    fn raw(&self) -> usize {
        self.0
    }

    fn range(&self) -> Range<VirtAddr> {
        self.start_vaddr()..self.start_vaddr().offset(MAX_SIZE).unwrap()
    }
}

impl TryFrom<usize> for Slot {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        let vaddr = VirtAddr::new((value * MAX_SIZE) as u64).map_err(|_| ())?;
        vaddr.try_into()
    }
}

impl TryFrom<VirtAddr> for Slot {
    type Error = ();

    fn try_from(value: VirtAddr) -> Result<Self, Self::Error> {
        if value.is_kernel() && !value.is_kernel_object_memory() {
            Err(())
        } else {
            Ok(Self(value.raw() as usize / MAX_SIZE))
        }
    }
}

/// Entries per thread, linear-scanned.
///
/// Was 2, on the plan's guess that a thread futexes in "very few slots -- its compartment heap,
/// plus perhaps a shared object". Measured (`slotmemo3`): 98% of misses were capacity, ~520 per
/// boot compulsory, so threads work materially more slots than that and were thrashing. 8 entries
/// is ~192 bytes per thread.
const SLOT_MEMO_LEN: usize = 8;

/// References resolved under one `regions` acquisition by
/// [`VirtContext::lookup_object_refs_cached`].
///
/// Sized off the data rather than guessed: multi-op `sys_thread_sync` calls carry ~10
/// virtual-referenced ops on average (26 352 ops across 2 620 such calls, `slotmemo3`), so 16
/// covers essentially all of them in one pass. It also bounds the stack cost of the resolution
/// array, which at the syscall's 1024-op limit would be ~32 KiB on top of the 24 KiB `unsleeps`
/// already there.
pub const RESOLVE_CHUNK: usize = 16;

/// Whether [`VirtContext::lookup_object_refs_cached`] consults the per-thread [`SlotMemo`] or
/// resolves every op through the plain per-slot lookup. The memo predates the `SlotMgr` refactor;
/// with the context-wide `regions` mutex gone this was its last runtime user.
///
/// Off: A/B at `syncab-on2`/`syncab-off` (one tree state, -j1) -- plain lookup ties or wins on
/// every bench (sleep_ready 135.8 -> 134.2, wake_no_waiters 234 -> 226, soft fault 959 -> 932 ns
/// means; ping_pong/map_unmap/contended a wash). With `FAULT_SLOT_MEMO` also off, nothing consults
/// the memo at runtime and the whole apparatus (`SlotMemo*`, `memo_tag`, `slotmemo` counters, the
/// per-thread field, both consts) is deletable per regionplan.md §6 -- left in place only so the
/// validated tree ships exactly the state the A/B measured.
pub const SYNC_SLOT_MEMO: bool = false;

/// Never reused, unlike `CONTEXT_IDS`. See [`VirtContext::memo_tag`].
static MEMO_TAGS: AtomicU64 = AtomicU64::new(1);

struct SlotMemoEntry {
    /// Which context filled this. Region liveness alone is not enough: it answers "is this region
    /// still alive", not "does *this* context still bind this slot to it", and the two differ
    /// exactly when a thread's context changes under it -- the region stays legitimately unremoved
    /// in the old context while the entry is consulted against the new one. That difference is a
    /// thread sleeping on the wrong word.
    ///
    /// The plan for this argued no context tag was needed, on the grounds that there is one real
    /// context. `sys_new_handle(_, HandleType::VmContext)` falsifies that with one syscall.
    tag: u64,
    slot: usize,
    /// Held rather than just its object, because `removed` on this region is the validity signal.
    /// Costs a pin on the region (and transitively its object) until the entry is replaced or
    /// cleared -- bounded at [`SLOT_MEMO_LEN`] regions per thread.
    region: Arc<MapRegion>,
    /// `clock` when this was last hit or filled, for LRU eviction.
    used_at: u64,
}

/// Slots remembered after eviction, purely to classify later misses. See
/// [`SlotMemoInner::was_evicted`].
const VICTIM_LOG: usize = 8;

struct SlotMemoInner {
    entries: [Option<SlotMemoEntry>; SLOT_MEMO_LEN],
    /// Per-thread monotonic tick. Only ordered against this thread's own entries, so wrapping is
    /// not a concern at u64 and no synchronization is needed beyond the enclosing spinlock.
    clock: u64,
    /// Slots this thread has evicted, most recent first-ish (ring). A cold miss on a slot in here
    /// is one a larger memo would have hit.
    victims: [usize; VICTIM_LOG],
    victim_pos: usize,
}

impl SlotMemoInner {
    /// Answer for `slot` if it is cached and still valid, refreshing its LRU stamp.
    ///
    /// Failed validation clears the entry here rather than leaving it for a later refill to
    /// overwrite: nothing sweeps entries any more, so a dead one would pin its region -- and
    /// transitively its object -- for as long as the thread lives.
    fn lookup(&mut self, slot: usize, tag: u64) -> Option<ObjectRef> {
        self.lookup_region(slot, tag).map(|r| r.object.clone())
    }

    /// As [`Self::lookup`], but handing back the region itself.
    ///
    /// The entry has always held the region -- it is what `removed` is read from -- and only
    /// `sys_thread_sync`'s caller wanted the object. The fault path wants the region, so it takes
    /// this and the object projection stays a one-line wrapper.
    fn lookup_region(&mut self, slot: usize, tag: u64) -> Option<Arc<MapRegion>> {
        self.clock += 1;
        let clock = self.clock;
        for entry in self.entries.iter_mut() {
            let Some(e) = entry else { continue };
            if e.slot != slot {
                continue;
            }
            if e.tag == tag && !e.region.removed.load(Ordering::Acquire) {
                e.used_at = clock;
                slotmemo::record_hit();
                return Some(e.region.clone());
            }
            *entry = None;
            slotmemo::record_invalidated();
            return None;
        }
        // No entry for this slot: distinguish "this thread evicted it recently, so a bigger memo
        // would have answered" from "genuinely not seen".
        if self.was_evicted(slot) {
            slotmemo::record_cold_capacity();
        } else {
            slotmemo::record_cold_compulsory();
        }
        None
    }

    fn was_evicted(&self, slot: usize) -> bool {
        self.victims.contains(&slot)
    }

    fn insert(&mut self, slot: usize, tag: u64, region: Arc<MapRegion>) {
        self.clock += 1;
        // Free entry, or the least recently used one. Round-robin was the first cut and evicts a
        // thread's hot slot as readily as a one-off; LRU is what keeps a small reused set resident
        // underneath a stream of slots touched once.
        let victim = match self.entries.iter().position(|e| e.is_none()) {
            Some(free) => free,
            None => {
                let (idx, evicted) = self
                    .entries
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, e)| e.as_ref().map(|e| e.used_at).unwrap_or(0))
                    .map(|(i, e)| (i, e.as_ref().map(|e| e.slot)))
                    .expect("slot memo is never empty");
                if let Some(evicted) = evicted {
                    self.victims[self.victim_pos] = evicted;
                    self.victim_pos = (self.victim_pos + 1) % VICTIM_LOG;
                }
                idx
            }
        };
        self.entries[victim] = Some(SlotMemoEntry {
            tag,
            slot,
            region,
            used_at: self.clock,
        });
    }
}

/// A per-thread memo of slot -> region, sitting in front of [`VirtContext::lookup_object_ref`]'s
/// sleeping `regions` mutex on the `sys_thread_sync` path.
///
/// Entries are validated per-region, against `MapRegion::removed`. The first version of this used
/// a per-*context* generation counter instead and managed a 14-22% hit rate: ~3300 mapping changes
/// per boot each invalidated every thread's memo for every slot, so an entry survived about ten
/// lookups. Per-region validation means a mapping change to an unrelated slot costs this thread
/// nothing -- which is also what lets a thread hit *during* `remove_object`'s long hold of
/// `regions`, the case the generation scheme could not serve by construction, since it invalidated
/// everything at the head of exactly that hold.
///
/// A `Spinlock` rather than the bare array the plan proposed: the entries own `ObjectRef`s, so a
/// torn read here is not a stale answer but an `Arc` clone off a half-written pointer. The nearest
/// precedent, [`crate::thread::sctx::SctxCache`], is a spinlock around a fixed array for the same
/// reason. The lock is per-thread and so never contended; what it costs against the mutex it
/// replaces is one uncontended atomic instead of an interval-tree walk under a sleeping lock.
pub struct SlotMemo {
    inner: Spinlock<SlotMemoInner>,
}

impl SlotMemo {
    pub const fn new() -> Self {
        Self {
            inner: Spinlock::new(SlotMemoInner {
                entries: [const { None }; SLOT_MEMO_LEN],
                clock: 0,
                // usize::MAX is not a valid slot (SLOTS is 1 << 17), so an unused log entry cannot
                // be mistaken for a real eviction.
                victims: [usize::MAX; VICTIM_LOG],
                victim_pos: 0,
            }),
        }
    }
}

impl Default for SlotMemo {
    fn default() -> Self {
        Self::new()
    }
}

const MAX_OPP_VEC: usize = 128;
struct ObjectPageProvider {
    pos: usize,
    inner_pos: usize,
    pages: heapless::Vec<(FrameRef, MappingSettings), MAX_OPP_VEC>,
}

impl ObjectPageProvider {
    pub fn new(pages: heapless::Vec<(FrameRef, MappingSettings), MAX_OPP_VEC>) -> Self {
        Self {
            pages,
            pos: 0,
            inner_pos: 0,
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages
            .iter()
            .skip(self.pos)
            .fold(0, |acc, x| acc + x.0.nr_pages())
            - self.inner_pos / PageNumber::PAGE_SIZE
    }
}

impl PhysAddrProvider for ObjectPageProvider {
    fn peek(&mut self) -> Option<PhysMapInfo> {
        let page = self.pages.get(self.pos)?;
        if page.0.nr_pages() > 1 {
            log::trace!(
                "peek: {:?}",
                page.0.start_address().offset(self.inner_pos).unwrap()
            );
        }
        Some(PhysMapInfo {
            addr: page.0.start_address().offset(self.inner_pos).unwrap(),
            len: PageNumber::PAGE_SIZE * page.0.nr_pages() - self.inner_pos,
            settings: page.1,
            // Only at the frame's own base: past that the offer is mid-frame, and the frame array
            // is indexed per 4 KiB, so `get_frame(addr)` would resolve to a different `Frame`.
            frame: (self.inner_pos == 0).then_some(page.0),
        })
    }

    fn consume(&mut self, mut len: usize) {
        if len > PageNumber::PAGE_SIZE {
            if len / PageNumber::PAGE_SIZE >= 512 {
                log::trace!("consume: {:?} ({} pages)", len, len / PageNumber::PAGE_SIZE);
            }
        }
        while len > 0 && self.pos < self.pages.len() {
            let rem_len =
                PageNumber::PAGE_SIZE * self.pages[self.pos].0.nr_pages() - self.inner_pos;
            if len < rem_len {
                self.inner_pos += len;
                break;
            } else {
                len = len.saturating_sub(rem_len);
                self.pos += 1;
                self.inner_pos = 0;
            }
        }
    }
}

/// Weak, and that is the fix for the largest leak this kernel has had: these were strong
/// `Arc`s with no removal path anywhere, so every user address space ever created was pinned
/// forever -- and with it its whole `RegionManager` of mappings and every object they
/// referenced. A spawn-storm suite measured ~16k dead compartments' contexts holding 92% of
/// RAM in pending-delete pages (pagerwedge.md §3.8). The entry removes itself in
/// [`VirtContext::drop`].
static ALL_CONTEXTS: Once<Mutex<BTreeMap<u64, Weak<VirtContext>>>> = Once::new();

fn get_all_contexts() -> &'static Mutex<BTreeMap<u64, Weak<VirtContext>>> {
    ALL_CONTEXTS.call_once(|| Mutex::new(BTreeMap::new()))
}

pub fn with_each_context(cb: impl FnMut(&Arc<VirtContext>)) {
    let all = get_all_contexts();
    let contexts = {
        let contexts = all.lock();
        contexts
            .values()
            .filter_map(|w| w.upgrade())
            .collect::<Vec<_>>()
    };
    contexts.iter().for_each(cb);
}

/// A/B: serve `with_arch`/`try_with_arch`/`for_each_arch` from the spinlock slot tree, running the
/// callback with no lock held at all. `false` restores taking the `secctx` sleeping mutex across
/// the callback, which is what every measurement before this was taken against.
///
/// The `secctx` tree stays either way: it serialises register/unregister, where a sleeping lock is
/// wanted (unregister walks every region and takes object page-table locks, which a spinlock could
/// not survive).
pub const SECCTX_LOCKFREE_ARCH: bool = true;

/// One security context's arch state within a [`VirtContext`], linked into two trees at once:
/// `secctx` under a sleeping mutex, and `target_cache` under a spinlock.
///
/// Sharing one allocation between them is the entire point. They used to be two `BTreeMap`s
/// holding separate copies of the same fact, and because filling a map allocates while
/// `target_cache` is a *spinlock*, register/unregister had to rebuild the whole target map off to
/// one side and swap it in. Linking a slot the caller already built costs nothing under either
/// lock, so the rebuild, the swap, and the window between them all go away.
struct SctxSlot {
    secctx_link: RBTreeAtomicLink,
    target_link: RBTreeAtomicLink,
    sctx: ObjID,
    arch: ArchContext,
    /// Callbacks running against `arch` right now, so teardown can wait them out.
    ///
    /// `with_arch` used to hold the `secctx` mutex across its callback, which made it mutually
    /// exclusive with [`VirtContext::unregister_sctx`]. Serving the callback from a snapshot
    /// removes that exclusion, and the `Arc` does not replace it: the `Arc` keeps the *allocation*
    /// alive, but an in-flight callback could still install mappings into an arch context whose
    /// teardown walk had already passed -- leaving them in a root that is then freed. See
    /// `unregister_sctx`, whose own comment explains why a freed root a recycled PCID can still
    /// name is worse than leaked frames.
    ///
    /// Only [`SlotGuard::drop`] ever decrements this. There is deliberately no manual path: the
    /// count is the mechanism the drain waits on, so a leaked decrement would make the drain read
    /// "nobody is using it" *while a callback runs*, which is precisely the bug it exists to
    /// prevent. Structuring the decrement as a guard is what keeps that unrepresentable rather
    /// than merely unlikely.
    users: AtomicUsize,
    /// Set by `unregister_sctx` once the drain has completed and the region walk is about to start
    /// tearing `arch` down.
    ///
    /// This is the independent witness for the drain, and it is deliberately not derived from
    /// `users`: checking `users == 0` after spinning on `users` tests nothing, because the counter
    /// is the mechanism under test. A guard still alive when this is set means the drain let a
    /// callback through, and [`SlotGuard::drop`] catches that -- at drop rather than at use, so it
    /// fires for *any* guard outliving the start of teardown rather than only for one that happens
    /// to touch `arch` at the wrong moment.
    torn_down: AtomicBool,
}

/// A slot borrowed for the duration of one callback. See [`SctxSlot::users`].
struct SlotGuard(Arc<SctxSlot>);

impl SlotGuard {
    fn arch(&self) -> &ArchContext {
        &self.0.arch
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        // Checked *before* the decrement: after it, teardown may proceed and this slot's page
        // tables may be freed, so this is the last moment the observation means anything.
        //
        // `assert!`, not `debug_assert!` -- release builds here set only `debug = true`, so a
        // `debug_assert` would be dead code that reads as coverage. The cost is one acquire load
        // per callback, against the sleeping-mutex acquire/release pair this change removes.
        assert!(
            !self.0.torn_down.load(Ordering::Acquire),
            "sctx slot began teardown while a callback still held it: the users drain let one through"
        );
        // Release, paired with the drain's Acquire load: everything the callback did to the page
        // tables must be visible to the teardown that is waiting on this reaching zero.
        self.0.users.fetch_sub(1, Ordering::Release);
    }
}

intrusive_adapter!(SecctxAdapter = Arc<SctxSlot>: SctxSlot { secctx_link: RBTreeAtomicLink });
impl<'a> KeyAdapter<'a> for SecctxAdapter {
    type Key = ObjID;
    fn get_key(&self, slot: &'a SctxSlot) -> ObjID {
        slot.sctx
    }
}

intrusive_adapter!(TargetAdapter = Arc<SctxSlot>: SctxSlot { target_link: RBTreeAtomicLink });
impl<'a> KeyAdapter<'a> for TargetAdapter {
    type Key = ObjID;
    fn get_key(&self, slot: &'a SctxSlot) -> ObjID {
        slot.sctx
    }
}

impl VirtContext {
    fn __new(kernel_arch: Option<ArchContext>) -> Self {
        let mut secctx = Mutex::new(RBTree::new(SecctxAdapter::NEW));
        // Nothing under this lock allocates: linking a slot the caller already built is the whole
        // critical section.
        secctx.set_safe_with_spinlocks(true);
        let new = Self {
            regions: RegionManager::default(),
            memo_tag: MEMO_TAGS.fetch_add(1, Ordering::Relaxed),
            is_kernel: kernel_arch.is_some(),
            id: CONTEXT_IDS.next(),
            secctx,
            kernel_arch,
            target_cache: Spinlock::new(RBTree::new(TargetAdapter::NEW)),
        };
        new
    }

    /// Construct a new context for the kernel.
    pub fn new_kernel() -> Arc<Self> {
        let this = Arc::new(Self::__new(Some(ArchContext::new_kernel())));
        let target = this.arch().target;
        // No `target_cache` entry: the kernel context has exactly one arch context, held in
        // `kernel_arch`, and `single_target` answers for it without touching the tree. That is the
        // same shape `single_arch` already uses, and it makes the kernel's switch lock-free.
        // Cache the root now, while we're safely outside the thread-switch path.
        KERNEL_ARCH_TARGET.call_once(|| target);
        let all = get_all_contexts();
        all.lock().insert(this.id.value(), Arc::downgrade(&this));
        this
    }

    /// Switch the calling processor to the kernel page tables.
    ///
    /// Deliberately lock-free, because the thread-switch path calls this: going through
    /// `switch_to` would take `target_cache`, nesting a spinlock acquisition inside the switch
    /// (which the lock tracker's single intent slot cannot represent, `locktrack.rs:192`) and
    /// serialising every processor that goes idle on one lock. A no-op during very early boot,
    /// before the kernel context exists -- there is nothing to switch away from yet.
    pub fn switch_to_kernel_context() {
        let Some(target) = KERNEL_ARCH_TARGET.poll() else {
            return;
        };
        let proc = tls_ready().then(current_processor);
        // Safety: the kernel context's root outlives every thread, and is never freed.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    /// Construct a new context for userspace.
    pub fn new() -> Arc<Self> {
        let this = Arc::new(Self::__new(None));
        // TODO: remove this once we have full support for user security contexts
        this.register_sctx(KERNEL_SCTX, ArchContext::new());
        let all = get_all_contexts();
        all.lock().insert(this.id.value(), Arc::downgrade(&this));
        this
    }

    /// The kernel context's one arch context. Panics on a user context; see
    /// [`VirtContext::kernel_arch`].
    fn arch(&self) -> &ArchContext {
        self.kernel_arch
            .as_ref()
            .expect("not the kernel context: its arch contexts live in `secctx`")
    }

    /// This context's arch state for `sctx`, without a lock, if there is only one of them.
    fn single_arch(&self, sctx: ObjID) -> Option<&ArchContext> {
        let arch = self.kernel_arch.as_ref()?;
        // Any other sctx on the kernel context missed the (empty) map before this existed, so
        // falling through to it keeps that answer -- `None` from `try_with_arch`, the `expect` from
        // `with_arch` -- rather than handing back the kernel's tables for something else. No caller
        // does this today; the assert is there to say so if one starts.
        debug_assert_eq!(sctx, KERNEL_SCTX);
        (sctx == KERNEL_SCTX).then_some(arch)
    }

    /// This context's switch target for `sctx`, without a lock, if there is only one of them.
    /// Mirrors [`VirtContext::single_arch`], including its fall-through for a non-kernel `sctx`.
    fn single_target(&self, sctx: ObjID) -> Option<ArchContextTarget> {
        let arch = self.kernel_arch.as_ref()?;
        debug_assert_eq!(sctx, KERNEL_SCTX);
        (sctx == KERNEL_SCTX).then_some(arch.target)
    }

    /// The slot for `sctx`, with a use counted against it, taken under the spinlock and returned
    /// with that lock released. The callback then runs holding no lock at all.
    ///
    /// The increment happens *under* the lock, not after it. `unregister_sctx` unlinks under this
    /// same lock and then waits for the count to drain, so an increment landing after the release
    /// could attach to a slot whose drain had already observed zero -- which is the whole race the
    /// count exists to close.
    fn borrow_arch_slot(&self, sctx: ObjID) -> Option<SlotGuard> {
        let slots = self.target_cache.lock();
        let slot = slots.find(&sctx).clone_pointer()?;
        slot.users.fetch_add(1, Ordering::Acquire);
        drop(slots);
        Some(SlotGuard(slot))
    }

    /// Bound on the snapshot [`Self::for_each_arch`] takes. A context has one arch context per
    /// attached security context, which is a handful in practice; overflow falls back to the
    /// mutex path rather than visiting a subset, since a partial walk here is a missed unmap.
    const ARCH_SNAPSHOT: usize = 32;

    /// Run `cb` against every arch context this context owns: the kernel's single one, or a user
    /// context's one per attached security context.
    fn for_each_arch(&self, cb: impl FnMut(&ArchContext)) {
        self.for_each_arch_in(None, cb)
    }

    /// [`Self::for_each_arch`], visiting only arches whose target is in `members` when the set is
    /// known complete. The filter runs *before* the `users` claim: a slot the caller would skip
    /// anyway costs one compare here instead of two RMWs and a guard round trip — measured as
    /// ~11 skipped claims per unmap (20.4M/boot, 94% of visits, `unmap census`). `None` degrades
    /// to visiting everything, exactly the contract the `members` set already carries at its one
    /// cb-side check — which callers keep as a second layer, so the overflow and mutex fallbacks
    /// (which still visit everything) rely on nothing new.
    fn for_each_arch_in(
        &self,
        members: Option<&[ArchContextTarget]>,
        mut cb: impl FnMut(&ArchContext),
    ) {
        if let Some(arch) = self.kernel_arch.as_ref() {
            cb(arch);
            return;
        }
        if SECCTX_LOCKFREE_ARCH {
            // Snapshot under the spinlock, iterate outside it: the only caller runs
            // `arch.unmap_object`, which does TLB shootdown and frame frees and cannot run with
            // interrupts masked. Same shape as the `members` set a few hundred lines below.
            let mut snap = heapless::Vec::<SlotGuard, { Self::ARCH_SNAPSHOT }>::new();
            let mut overflow = false;
            {
                let slots = self.target_cache.lock();
                let mut cursor = slots.front();
                while let Some(slot) = cursor.clone_pointer() {
                    if let Some(members) = members
                        && !members.contains(&slot.arch.target)
                    {
                        // Counted here so the census's skip total keeps meaning "arches the
                        // membership filter excluded", wherever the filter runs.
                        unmap_census::record_skip();
                        cursor.move_next();
                        continue;
                    }
                    slot.users.fetch_add(1, Ordering::Acquire);
                    if snap.push(SlotGuard(slot)).is_err() {
                        overflow = true;
                        break;
                    }
                    cursor.move_next();
                }
            }
            if !overflow {
                for guard in &snap {
                    cb(guard.arch());
                }
                return;
            }
            // More matching contexts than the snapshot holds. Drop what we took and fall through
            // to the mutex path, which visits everything (the cb-side membership check covers it).
            drop(snap);
        }
        for slot in self.secctx.lock().iter() {
            cb(&slot.arch);
        }
    }

    pub fn try_with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> Option<R> {
        if let Some(arch) = self.single_arch(sctx) {
            return Some(cb(arch));
        }
        if SECCTX_LOCKFREE_ARCH {
            let guard = self.borrow_arch_slot(sctx)?;
            return Some(cb(guard.arch()));
        }
        let secctx = self.secctx.lock();
        secctx.find(&sctx).get().map(|slot| cb(&slot.arch))
    }

    pub fn with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> R {
        if let Some(arch) = self.single_arch(sctx) {
            return cb(arch);
        }
        if SECCTX_LOCKFREE_ARCH {
            let guard = self
                .borrow_arch_slot(sctx)
                .expect("cannot get arch mapper for unattached security context");
            return cb(guard.arch());
        }
        let secctx = self.secctx.lock();
        cb(&secctx
            .find(&sctx)
            .get()
            .expect("cannot get arch mapper for unattached security context")
            .arch)
    }

    /// Page-table frames [`Self::map_object`] can need to map one slot.
    ///
    /// The same count for every slot, which is what lets a caller charge before it has picked one
    /// (`insert_kernel_object` does). A slot is `MAX_SIZE` long and `MAX_SIZE`-aligned, so at each
    /// level it either covers whole tables from offset zero, or -- above `MAX_SIZE` -- sits wholly
    /// inside one entry, however far into it. Neither term depends on which slot.
    /// `test_slot_map_precharge_is_slot_independent` pins that.
    fn slot_map_tables() -> usize {
        MappingCursor::new(VirtAddr::start_user_memory(), MAX_SIZE)
            .max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1)
    }

    /// Charge `fa` with the frames [`Self::slot_map_tables`] counts.
    ///
    /// Callers run this *before* taking the `regions` lock. `WAIT_OK` parks the thread until the
    /// reclaimer frees memory, and `regions` is on the fault path of the whole context
    /// (`fault::get_map_region`), so waiting for memory under it stalls every fault in that context
    /// for the duration. `FrameAllocator::precharge_nowait` names the same rule.
    fn precharge_slot_map(fa: &mut FrameAllocator) {
        fa.precharge(Self::slot_map_tables(), FrameAllocFlags::WAIT_OK);
    }

    pub fn map_object(&self, info: &MapRegion, fa: &mut FrameAllocator) {
        // An explicit target wins; zero means "whatever this thread is running as", which for the
        // monitor is KERNEL_SCTX -- its instance id is zero too. That now resolves (see
        // `security::kernel_sctx`), so those mappings get installed here rather than left to the
        // fault path.
        let sctx = if self.is_kernel {
            // The kernel context has exactly one arch context, registered under KERNEL_SCTX by
            // `new_kernel`, and nothing ever registers another into it. Taking the caller's active
            // sctx here would just make `try_with_arch` miss and silently install nothing --
            // which is what every `insert_kernel_object` from a thread in a real context did.
            KERNEL_SCTX
        } else if info.target_sctx.raw() != 0 {
            info.target_sctx
        } else {
            current_thread_ref()
                .map(|ct| ct.active_sctx_id())
                .unwrap_or(KERNEL_SCTX)
        };

        let len = info.range.end - info.range.start;
        let cursor = MappingCursor::new(info.range.start, len);
        use mapobjprofile as mp;
        let t_total = mp::start();
        // Reading the thread's own `secctx.active()` instead of `get_sctx(active_id())` is faster
        // (68% of this function). The two used to differ -- `get_sctx(0)` returned `Err` and
        // skipped this whole block -- but both now resolve to the single `kernel_sctx()`, so the
        // swap is available if this shows up in a profile again. See pagerperf.md 17.
        let t = mp::start();
        let sctx = crate::security::get_sctx(sctx);
        mp::record(mp::Stage::Sctx, t);
        if let Ok(sctx) = sctx {
            let t = mp::start();
            let perms = sctx.lookup(info.object().id(), info.default_prot);
            mp::record(mp::Stage::Lookup, t);
            let t = mp::start();
            let mut pt = if info.stable.is_some() {
                PtGuard::new(info.stable.as_ref().unwrap())
            } else {
                info.object.lock_page_tables()
            };
            mp::record(mp::Stage::PtLock, t);
            let t_arch = mp::start();
            self.try_with_arch(sctx.id(), |arch| {
                let t = mp::start();
                pt.add_invalidate(arch.target, cursor);
                mp::record(mp::Stage::AddInv, t);
                let t = mp::start();
                let settings = MappingSettings::new(
                    perms.effective(info.default_prot, info.prot),
                    info.cache_type,
                    MappingFlags::USER,
                );
                let took_ref = arch.object_map(cursor, &mut *pt, settings, fa);
                // Only a region holding the object's *own* tables charges the object. A stable
                // region works on a clone, and the unmap paths mirror this exactly -- their
                // `counted` is `stable.is_none()` -- so charging here would raise a count that
                // nothing ever lowers and leave the object permanently unreapable. Before the
                // count moved onto `Object` this fell out for free: the increment landed on
                // whichever `ObjectPageTable` was in hand, and for a clone that field was never
                // read by anything.
                if took_ref && info.stable.is_none() {
                    // Under `pt`, the page-table lock the count's field doc requires.
                    info.object().inc_map_count();
                    info.object().inc_sites[0]
                        .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    info.object().inc_sctx.store(
                        sctx.id().raw() as u64,
                        core::sync::atomic::Ordering::Relaxed,
                    );
                }
                mp::record(mp::Stage::ObjMap, t);
            });
            mp::record(mp::Stage::Arch, t_arch);
            mp::record(mp::Stage::Total, t_total);
        };
    }

    /// `obj` is the owner of `object_tables`, needed only to charge the map count; the caller
    /// holds its page-table lock as `object_tables`. `None` when `object_tables` is a stable
    /// region's clone rather than the object's own tables -- see the note in [`Self::map_object`]
    /// for why such a mapping takes no count.
    pub fn ensure_object_mapped(
        &self,
        sctxid: ObjID,
        obj: Option<&crate::obj::Object>,
        cursor: MappingCursor,
        object_tables: &mut ObjectPageTable,
        settings: MappingSettings,
    ) -> bool {
        // Ask before charging for it. Every fault on an already-resident page reaches here, and
        // only a couple of percent of them install anything, so the frame allocator below was
        // mostly being taken, precharged, and put back to discover there was nothing to do.
        if self.try_with_arch(sctxid, |arch| arch.is_object_mapped(cursor, settings)) == Some(true)
        {
            return false;
        }
        let mut fa = take_or_new_frame_allocator();
        fa.precharge(
            cursor.max_number_new_tables(Table::top_level(), ObjectPageTable::top_level() - 1),
            FrameAllocFlags::WAIT_OK,
        );
        self.with_arch(sctxid, |arch| {
            object_tables.add_invalidate(arch.target, cursor);
            match arch.ensure_object_mapped(cursor, object_tables, settings, &mut fa) {
                Some(took_ref) => {
                    if took_ref && let Some(obj) = obj {
                        obj.inc_map_count();
                        obj.inc_sites[1].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    }
                    true
                }
                None => false,
            }
        })
    }

    pub fn print_objects(&self) {
        for obj in self.regions.objects() {
            log!("{} => ", obj);
            if let Ok(obj) = lookup_object(obj, LookupFlags::empty()).ok_or(()) {
                for mapping in obj.mappings() {
                    log!("{:?}, ", mapping.range);
                }
            }
            logln!("");
        }
    }

    pub fn register_sctx(&self, sctx: ObjID, arch: ArchContext) {
        if self.kernel_arch.is_some() {
            // The kernel context's one arch context is a field, installed when it was built.
            debug_assert_eq!(sctx, KERNEL_SCTX);
            return;
        }
        // Built before either lock is taken. This is the only allocation the whole operation
        // makes, and moving it here is what removes the rebuild-and-swap: linking the same slot
        // into both trees allocates nothing, so the spinlock no longer bounds what we may do.
        let slot = Arc::new(SctxSlot {
            secctx_link: RBTreeAtomicLink::default(),
            target_link: RBTreeAtomicLink::default(),
            sctx,
            arch,
            users: AtomicUsize::new(0),
            torn_down: AtomicBool::new(false),
        });
        // Slot tree first, `secctx` second -- the reverse of the original order, and it matters
        // now that lookups read the slot tree: inserting there last would leave a window in which
        // a registered sctx is invisible to `with_arch`, which panics on a miss. The duplicate
        // check moves here with it, so one lock decides the race.
        //
        // The flag rather than an early return inside the block: losing the race drops `slot`, and
        // with it an `ArchContext` whose drop frees a root page table. That must not happen under
        // a spinlock.
        let dup = {
            let mut slots = self.target_cache.lock();
            if slots.find(&sctx).is_null() {
                slots.insert(slot.clone());
                false
            } else {
                true
            }
        };
        if dup {
            return;
        }
        self.secctx.lock().insert(slot);
    }

    pub fn unregister_sctx(&self, sctx: ObjID) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        // Retire the target *before* `arch` is dropped at the end of this function, not after: the
        // drop frees the root page table and releases the PCID, and until the target is gone a
        // concurrent `switch_to` on this sctx would still find it and load it. That keeps a
        // recycled PCID from being installed against a freed root -- which would alias one address
        // space's translations onto whoever gets the PCID next. Unlinking is all this takes now,
        // so the window is the unlink itself rather than a rebuild.
        //
        // Unlinked from the slot tree *first*, before `secctx`: that tree is what
        // `borrow_arch_slot` reads, so removing it there is what stops new callbacks from finding
        // this slot. Doing it
        // in the old order would leave the drain below racing lookups it had already passed.
        let removed = self.target_cache.lock().find_mut(&sctx).remove();

        let slot = {
            let mut secctx = self.secctx.lock();
            let Some(slot) = secctx.find_mut(&sctx).remove() else {
                // No arch state registered here -- the common case at `SecurityContext::drop`
                // time, since the arch slot is usually torn down earlier. Nothing to tear down,
                // and the regions are not this function's to touch: they are released by the
                // monitor dropping its `MapHandle`s, which is refcounted and knows about the
                // other compartments sharing them.
                drop(secctx);
                drop(removed);
                return;
            };
            slot
        };
        drop(removed);

        if SECCTX_LOCKFREE_ARCH {
            // Unlinked above, so no new callback can find this slot; wait out the ones already
            // running before the region walk below starts tearing their page tables down. Rare --
            // this runs from `SecurityContext::drop` -- so a spin is the right shape.
            //
            // Cannot deadlock against itself: the only caller is that destructor, reached via
            // `with_each_context`, which iterates outside the ALL_CONTEXTS mutex; and no
            // `with_arch` callback touches a `SecurityContextRef`, so no thread can be
            // inside one while dropping the last reference to the same context. The
            // wait is bounded by callback duration.
            // Yields rather than spinning bare, and that distinction is load-bearing. A
            // `SlotGuard` is held across a callback that does real work -- `arch.object_map`, TLB
            // batching -- so a timer can preempt its holder mid-callback. A pure spin here then
            // never lets that holder run again, which deadlocked the single-vcpu test boot at
            // `st` (schedtest, thread spawn/join churn): 36 of 55 tests, then silence. Measured,
            // not theorised -- the same boot with `SECCTX_LOCKFREE_ARCH = false` ran 55/55.
            //
            // The mutex arm has no such hazard by construction: it *blocks* on `secctx`, and
            // blocking yields the cpu. Replacing a blocking wait with a busy wait is what
            // introduced this, which is the general hazard in the change, not an incidental bug.
            spin_wait_until(
                || (slot.users.load(Ordering::Acquire) == 0).then_some(()),
                || schedule(SchedFlags::YIELD | SchedFlags::PREEMPT | SchedFlags::REINSERT),
            );
            // After the drain, before the walk: from here on any guard still alive is a drain
            // failure, and `SlotGuard::drop` says so. See `SctxSlot::torn_down`.
            slot.torn_down.store(true, Ordering::Release);
        }

        {
            let arch = &slot.arch;
            for region in self.regions.mappings() {
                let cursor = region.mapping_cursor(0, MAX_SIZE);
                // Whichever tree backs this region, as in remove_object: a stable clone still has
                // to be told its mapping is gone, even though it never took a count against the
                // object and so has nothing to give back.
                let mut pt = if let Some(stable) = region.stable.as_ref() {
                    PtGuard::new(stable)
                } else {
                    region.object().lock_page_tables()
                };
                let counted = region.stable.is_none();
                let obj_table = counted.then(|| pt.context_table_addr()).flatten();
                let released = arch.unmap_object(cursor, obj_table, &mut fa);
                pt.remove_invalidate(arch.target, cursor);
                if pt.take_latch_notice() {
                    crate::obj::pagetables::invl_overflow::note_object(
                        region.object().id(),
                        pt.invls_live(),
                        pt.invls_len(),
                    );
                }
                let last = if counted && released {
                    if region.object().dec_map_count() == 0 {
                        region.object().note_last_unmap();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                drop(pt);
                if crate::obj::TARGETED_REAP && last && region.object().is_pending_delete() {
                    crate::obj::request_reap(region.object());
                }
            }
        }
    }

    /// Init a context for being the kernel context, and clone the mappings from the bootstrap
    /// context.
    pub(super) fn init_kernel_context(&self) {
        let proto = unsafe { Mapper::current() };
        let rm = proto.readmap(MappingCursor::new(
            VirtAddr::start_kernel_memory(),
            usize::MAX,
        ));
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::WAIT_OK | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        for map in rm.coalesce() {
            let cursor = MappingCursor::new(map.vaddr(), map.len());
            let settings = MappingSettings::new(
                map.settings().perms(),
                map.settings().cache(),
                map.settings().flags() | MappingFlags::GLOBAL | MappingFlags::WIRED,
            );
            let mut phys = ContiguousProvider::new(map.paddr(), map.len(), settings);
            self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));
        }

        // ID-map the lower memory. This is needed by some systems to boot secondary CPUs. This
        // mapping is cleared by the call to prep_smp later.
        let id_len = 0x100000000; // 4GB
        let cursor = MappingCursor::new(
            VirtAddr::new(
                Table::level_to_page_size(Table::last_level())
                    .try_into()
                    .unwrap(),
            )
            .unwrap(),
            id_len,
        );
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE | Protections::EXEC,
            CacheType::WriteBack,
            MappingFlags::WIRED,
        );
        let mut phys = ContiguousProvider::new(
            PhysAddr::new(
                Table::level_to_page_size(Table::last_level())
                    .try_into()
                    .unwrap(),
            )
            .unwrap(),
            id_len,
            settings,
        );

        self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));

        let cursor = MappingCursor::new(VirtAddr::PHYS_START, PhysAddr::phys_mem_map_len());
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE | Protections::EXEC,
            CacheType::WriteBack,
            MappingFlags::WIRED,
        );
        let mut phys = ContiguousProvider::new(
            PhysAddr::new(0).unwrap(),
            PhysAddr::phys_mem_map_len(),
            settings,
        );

        self.with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys, &mut fa));
    }

    /// [`UserContext::lookup_object_ref`], consulting the calling thread's [`SlotMemo`] first.
    ///
    /// For `sys_thread_sync`, which asks this question once per virtual-referenced op and is the
    /// busiest syscall in the system. A hit costs an uncontended per-thread spinlock, one acquire
    /// load of the region's `removed` flag, and one `Arc` clone; a miss costs that plus the
    /// ordinary locked path, and refills.
    pub fn lookup_object_ref_cached(&self, info: Slot) -> Option<ObjectRef> {
        let mut out = [None];
        self.lookup_object_refs_cached(&[info], &mut out);
        out[0].take()
    }

    /// [`Self::lookup_object_ref_cached`] for several slots, taking each lock once for the batch
    /// rather than once per slot.
    ///
    /// `sys_thread_sync` resolves every op in a call independently, so a call carrying `n`
    /// virtual-referenced ops takes `regions` `n` times. Measured (`slotmemo3`): only 7-13% of
    /// calls carry more than one such op, but those calls carry most of the ops, and 61-65% of all
    /// virtual-referenced ops are a sibling's acquisition away from being free.
    ///
    /// Whatever the memo answers costs no `regions` acquisition at all, so the lock is taken only
    /// if something misses, and then exactly once.
    ///
    /// All of the above was written against the context-wide `regions` mutex. With `SlotMgr`
    /// underneath, `lookup_region` takes a per-slot shard spinlock, so there is no single
    /// acquisition left to amortize -- [`SYNC_SLOT_MEMO`] is the A/B switch for whether the memo
    /// still pays.
    pub fn lookup_object_refs_cached(&self, slots: &[Slot], out: &mut [Option<ObjectRef>]) {
        assert_eq!(slots.len(), out.len());
        out.fill(None);

        if !SYNC_SLOT_MEMO {
            for (i, slot) in slots.iter().enumerate() {
                out[i] = self.lookup_object_ref(*slot);
            }
            return;
        }

        // Slots this context cannot answer for itself: kernel object memory reaches a *user*
        // context's `lookup_object_ref` only to be rerouted to `kernel_context()`, so a memo or a
        // `regions` walk here would answer from the wrong context. The kernel context itself owns
        // those mappings and takes the ordinary path.
        //
        // Computed once and reused by every loop below. Restating the predicate per loop is what
        // broke `batch-lru`: two of the three dropped the `&& !self.is_kernel` half, so a kernel
        // thread syncing on a kernel object -- `queue.rs`'s pager queue, at boot -- was skipped by
        // every phase and fell out as InvalidAddress.
        let reroute = |slot: &Slot| slot.start_vaddr().is_kernel_object_memory() && !self.is_kernel;

        let mut any_local = false;
        for (i, slot) in slots.iter().enumerate() {
            if reroute(slot) {
                slotmemo::record_skip();
                out[i] = self.lookup_object_ref(*slot);
            } else {
                any_local = true;
            }
        }
        if !any_local {
            return;
        }
        let Some(thread) = current_thread_ref() else {
            for (i, slot) in slots.iter().enumerate() {
                if !reroute(slot) {
                    slotmemo::record_skip();
                    out[i] = self.lookup_object_ref(*slot);
                }
            }
            return;
        };

        // One memo acquisition for the batch, not one per slot.
        {
            let mut memo = thread.slot_memo.inner.lock();
            for (i, slot) in slots.iter().enumerate() {
                if out[i].is_some() || reroute(slot) {
                    continue;
                }
                out[i] = memo.lookup(slot.raw(), self.memo_tag);
            }
        }

        if out.iter().all(|o| o.is_some()) {
            return;
        }

        // Everything that missed.
        let mut resolved: [Option<Arc<MapRegion>>; RESOLVE_CHUNK] = [const { None }; RESOLVE_CHUNK];
        {
            let mut looked_up = 0;
            for (i, slot) in slots.iter().enumerate() {
                if out[i].is_some() || reroute(slot) {
                    continue;
                }
                looked_up += 1;
                resolved[i] = self.regions.lookup_region(*slot);
            }
            // Realized, not hypothetical: `sync batching`'s saveable count is computed at syscall
            // entry and reports the same number whether this function batches or not. This counts
            // acquisitions actually taken against slots actually resolved under them, so the
            // difference is the saving that happened.
            slotmemo::record_batch(looked_up);
        }

        let mut memo = thread.slot_memo.inner.lock();
        for (i, slot) in slots.iter().enumerate() {
            let Some(region) = resolved[i].take() else {
                continue;
            };
            out[i] = Some(region.object.clone());
            memo.insert(slot.raw(), self.memo_tag, region);
        }
    }

    /// The region backing `slot` in this context, consulting the calling thread's [`SlotMemo`]
    /// first.
    ///
    /// For the page-fault path, which took `regions` on *every* fault purely to find the region
    /// and clone it. Measured at smp4 with four threads faulting concurrently on four separate
    /// objects, that stage went from 155 ns to 7.5 us per fault -- 58% of the whole contended
    /// increase -- while the per-object page-table lock stayed flat. A convoy on a lock nobody
    /// needed to hold: the answer is per-slot and the threads shared nothing but the context.
    ///
    /// Safe to answer from a memo for the same reason `sys_thread_sync` can: the entry is
    /// validated against this context's `memo_tag` and the region's own `removed` flag. The fault
    /// path already had to tolerate a stale region -- it clones one out from under the lock and
    /// re-checks `removed` before installing a mapping (see `MapRegion::handle_fault`) -- so this
    /// widens an existing window rather than opening a new one.
    /// The fault path's two regions -- the faulting address's and the one executing at `ip` --
    /// taking the memo once and `regions` at most once.
    ///
    /// Batched for the same reason [`Self::lookup_object_refs_cached`] is, and measured the same
    /// way. Resolving them independently costs two per-thread spinlock round trips per fault
    /// instead of one, and each of those disables and restores interrupts; against the *single*
    /// `regions` acquisition this replaces, that was a 7% regression on the uncontended fault even
    /// while it took 26% off the contended one. One acquisition in, one out.
    pub fn lookup_fault_regions(
        &self,
        slot: Slot,
        exec_slot: Option<Slot>,
    ) -> (Option<Arc<MapRegion>>, Option<Arc<MapRegion>>) {
        // Kernel object memory is not this context's to answer; the caller checks the kernel
        // context itself. Such a slot is never memoized -- an entry for it could only ever miss,
        // and would evict a live one.
        let local = |s: &Slot| !(s.start_vaddr().is_kernel_object_memory() && !self.is_kernel);
        let Some(thread) = current_thread_ref().filter(|_| local(&slot)) else {
            slotmemo::record_skip();
            return (
                self.lookup_slot(slot.raw()),
                exec_slot.and_then(|s| self.lookup_slot(s.raw())),
            );
        };
        let exec_slot = exec_slot.filter(local);

        let (mut region, mut exec) = {
            let mut memo = thread.slot_memo.inner.lock();
            (
                memo.lookup_region(slot.raw(), self.memo_tag),
                exec_slot.and_then(|s| memo.lookup_region(s.raw(), self.memo_tag)),
            )
        };
        if region.is_some() && (exec.is_some() || exec_slot.is_none()) {
            return (region, exec);
        }

        // Whatever missed.
        if region.is_none() {
            region = self.regions.lookup_region(slot);
        }
        if exec.is_none() {
            exec = exec_slot.and_then(|s| self.regions.lookup_region(s));
        }

        let mut memo = thread.slot_memo.inner.lock();
        if let Some(r) = &region {
            memo.insert(slot.raw(), self.memo_tag, r.clone());
        }
        if let (Some(s), Some(r)) = (exec_slot, &exec) {
            memo.insert(s.raw(), self.memo_tag, r.clone());
        }
        (region, exec)
    }

    pub fn lookup_region_cached(&self, slot: Slot) -> Option<Arc<MapRegion>> {
        // Kernel object memory is not this context's to answer. The caller checks the kernel
        // context itself when this returns None, so take the plain path and do not memoize a slot
        // this context can only ever miss on -- an entry for it would evict a live one.
        if slot.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            slotmemo::record_skip();
            return self.lookup_slot(slot.raw());
        }
        let Some(thread) = current_thread_ref() else {
            slotmemo::record_skip();
            return self.lookup_slot(slot.raw());
        };
        if let Some(region) = thread
            .slot_memo
            .inner
            .lock()
            .lookup_region(slot.raw(), self.memo_tag)
        {
            return Some(region);
        }
        let region = self.lookup_slot(slot.raw())?;
        thread
            .slot_memo
            .inner
            .lock()
            .insert(slot.raw(), self.memo_tag, region.clone());
        Some(region)
    }

    pub fn lookup_slot(&self, slot: usize) -> Option<Arc<MapRegion>> {
        self.regions.lookup_region(Slot::try_from(slot).ok()?)
    }

    /// Fill `buf` with the numbers of the slots that have something mapped in them, ascending,
    /// skipping the first `offset`. Returns how many were written; short of `buf.len()` means the
    /// enumeration is done. Backs `sys_enumerate_slots`.
    pub fn enumerate_slots(&self, buf: &mut [u64], offset: usize) -> Result<usize, TwzError> {
        let mut slots = self
            .regions
            .mappings()
            .iter()
            .filter_map(|region| {
                Slot::try_from(region.range.start)
                    .ok()
                    .map(|s| s.raw() as u64)
            })
            .collect::<Vec<_>>();
        slots.sort_unstable();
        slots.dedup();

        let count = slots.len().saturating_sub(offset).min(buf.len());
        buf[..count].copy_from_slice(&slots[offset..(offset + count)]);
        Ok(count)
    }
}

impl UserContext for VirtContext {
    type MappingInfo = Slot;
    type SwitchTarget = ArchContextTarget;

    fn switch_target(&self, sctx: ObjID) -> Option<ArchContextTarget> {
        if let Some(target) = self.single_target(sctx) {
            return Some(target);
        }
        self.target_cache
            .lock()
            .find(&sctx)
            .get()
            .map(|slot| slot.arch.target)
    }

    unsafe fn switch_to_target(&self, target: &ArchContextTarget) {
        let proc = tls_ready().then(current_processor);
        // Safety: the caller guarantees the target is still registered here.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    fn switch_to(&self, sctx: ObjID) {
        //let sctx = 0.into();
        if let Some(target) = self.single_target(sctx) {
            let proc = tls_ready().then(current_processor);
            // Safety: the kernel context's root outlives every thread and is never freed.
            unsafe {
                ArchContext::switch_to_target(&target, proc);
            }
            return;
        }
        let tc = self.target_cache.lock();
        let target = &tc
            .find(&sctx)
            .get()
            .expect("tried to switch to a non-registered sctx")
            .arch
            .target;
        // TLS/the processor registry isn't up yet during the very early boot switch from
        // memory::init(); pass None in that case rather than looking up current_processor()
        // from inside the arch-specific switch code.
        let proc = tls_ready().then(current_processor);
        // Safety: we get the target from an ArchContext that we track.
        unsafe {
            ArchContext::switch_to_target(target, proc);
        }
    }

    fn insert_object(
        self: &Arc<Self>,
        slot: Slot,
        object_info: &ObjectContextInfo,
    ) -> Result<(), TwzError> {
        let _guard = mapprofile::Timer(mapprofile::stats_stamp());
        let t_total = mapprofile::start();
        log::debug!(
            "insert {} to {:?} {:?}",
            object_info.object.id(),
            slot.start_vaddr(),
            object_info.prot(),
        );

        let t_stable = mapprofile::start();
        let mut stable = None;
        if object_info.flags.contains(MapFlags::STABLE) {
            stable = Some(Arc::new(Mutex::new(
                object_info.object().cow_clone_page_tables()?,
            )));
        }
        mapprofile::record(mapprofile::Stage::Stable, t_stable);

        let t_check = mapprofile::stats_stamp();
        let (_is_ok, default_prot) = object_info.object.check_id();
        mapprofile::record_checkid(if mapprofile::MAP_STATS {
            (crate::instant::Instant::now() - t_check).as_nanos() as u64
        } else {
            0
        });
        let t_region = mapprofile::start();
        let new_slot_info = MapRegion {
            prot: object_info.prot(),
            cache_type: object_info.cache(),
            object: object_info.object().clone(),
            offset: 0,
            range: slot.range(),
            flags: object_info.flags,
            target_sctx: object_info.target_sctx(),
            stable,
            default_prot,
            should_sync: AtomicBool::new(false),
            removed: AtomicBool::new(false),
        };

        mapprofile::record(mapprofile::Stage::Region, t_region);

        // Ahead of the lock: see `precharge_slot_map`.
        let t_pre = mapprofile::start();
        let mut fa = take_or_new_frame_allocator();
        Self::precharge_slot_map(&mut fa);
        mapprofile::record(mapprofile::Stage::Precharge, t_pre);

        // Claim the slot before mapping, and hold the claim across the map: otherwise a racing
        // insert can clobber our object table entry, and a Busy return leaves behind a mapping
        // plus the map count taken for it, which keeps the object from ever being reaped. The
        // claim is a per-slot state rather than a held lock -- `map_object` takes an object's
        // page-table lock, which is a sleeping mutex. See `SlotState`.
        let t_lock = mapprofile::start();
        let guard = self.regions.begin_insert(slot)?;
        mapprofile::record(mapprofile::Stage::Lock, t_lock);
        // Registered with the object *before* the install takes the map count: `is_reapable`
        // treats "count > 0 with no live mapping" as stale accounting, so the mapping must be
        // visible whenever the count is. The old order (install, then register) left a window
        // where a mid-map object looked stale.
        let t_map = mapprofile::start();
        let region = Arc::new(new_slot_info);
        region.object().add_mapping(slot.raw(), &region);
        self.map_object(&region, &mut fa);
        mapprofile::record(mapprofile::Stage::MapObj, t_map);
        let t_ins = mapprofile::start();
        guard.commit(region);
        mapprofile::record(mapprofile::Stage::Insert, t_ins);
        mapprofile::record(mapprofile::Stage::Total, t_total);
        Ok(())
    }

    fn lookup_object(&self, info: Self::MappingInfo) -> Option<ObjectContextInfo> {
        if info.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            kernel_context().lookup_object(info)
        } else {
            self.regions.lookup_region(info).map(|info| (&*info).into())
        }
    }

    fn lookup_object_ref(&self, info: Self::MappingInfo) -> Option<ObjectRef> {
        if info.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            kernel_context().lookup_object_ref(info)
        } else {
            self.regions
                .lookup_region(info)
                .map(|region| region.object().clone())
        }
    }

    fn remove_object(&self, info: Self::MappingInfo) {
        self.remove_object_from(info, unmapprofile::Initiator::Own);
    }
}

impl VirtContext {
    /// [`UserContext::remove_object`] with the initiator named, for [`unmapprofile::UNMAP_HIST`].
    /// The trait method forwards with [`unmapprofile::Initiator::Own`]; callers that know better
    /// (the handle form of `sys_object_unmap`) call this directly.
    pub fn remove_object_from(&self, info: Slot, initiator: unmapprofile::Initiator) {
        use unmapprofile::Stage as UStage;
        let t_hist = unmapprofile::hist_stamp();
        let t_total = unmapprofile::start();
        let t = unmapprofile::start();
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let Some((slot, guard)) = self.regions.begin_remove(info) else {
            return;
        };
        unmapprofile::record(UStage::Pre, t);
        let t = unmapprofile::start();
        slot.object().remove_mapping(info.raw());
        fault::note_unmap(info.raw(), slot.object());
        unmapprofile::record(UStage::Notify, t);

        // The slot stays claimed for the whole teardown: insert_object claims a free slot and maps
        // immediately (see there), so releasing it here would let another object be mapped into
        // this slot and then have its entry removed by the unmap below. A claim rather than a held
        // lock because the teardown takes sleeping mutexes -- see `SlotState`.
        {
            // Whichever page tables the fault path would use for this region -- taking the same
            // one is what makes the `removed` store below and that path's check of it ordered.
            let t = unmapprofile::start();
            let mut pt = if let Some(stable) = slot.stable.as_ref() {
                PtGuard::new(stable)
            } else {
                slot.object().lock_page_tables()
            };
            unmapprofile::record(UStage::Lock, t);
            let t_arches = unmapprofile::start();
            // An in-flight fault now either mapped before us, and the unmap below undoes it, or
            // sees this and does not map at all. See MapRegion::handle_fault.
            slot.removed
                .store(true, core::sync::atomic::Ordering::SeqCst);

            // Stable regions map a private clone of the object's tables, and never took a count
            // against the object (see map_object), so there is nothing to give back for them.
            let counted = slot.stable.is_none();
            let obj_table = counted.then(|| pt.context_table_addr()).flatten();
            let mut n_arches = 0usize;
            let mut n_mapped = 0usize;
            // Stage 3: iterate the contexts that actually hold this object rather than every
            // attached one. The cost being avoided is not the walk -- it is `unmap_object`'s
            // per-context mapper spinlock, taken 45k times a boot to find nothing 94% of the time
            // (see unmap.md). So membership filters *before* that call and the walk itself stays.
            //
            // Copied out rather than borrowed because the loop body needs `pt` mutably. `None` here
            // means the set is not known complete, and then this must degrade to exactly the old
            // behaviour -- visiting everything -- which is what makes a wrong membership set a
            // wasted acquisition rather than a missed unmap.
            let t_mem = unmapprofile::start();
            let members: Option<heapless::Vec<ArchContextTarget, 32>> = (counted)
                .then(|| pt.members().map(|m| m.iter().copied().collect()))
                .flatten();
            unmapprofile::record(UStage::Members, t_mem);
            self.for_each_arch_in(members.as_deref(), |arch| {
                // Second layer behind for_each_arch_in's pre-filter: this is what the overflow
                // and mutex fallback paths (which visit everything) rely on.
                if let Some(members) = members.as_ref()
                    && !members.contains(&arch.target)
                {
                    unmap_census::record_skip();
                    return;
                }
                let t_uo = unmapprofile::start();
                let cursor = slot.mapping_cursor(0, MAX_SIZE);
                let released = arch.unmap_object(cursor, obj_table, &mut fa);
                unmapprofile::record(UStage::UnmapObj, t_uo);
                let t_ri = unmapprofile::start();
                n_arches += 1;
                if released {
                    n_mapped += 1;
                }
                if counted {
                    // Stage 2's validation, and the reason the stage exists: this arch just
                    // released a mapping of this object, so a complete membership set must have
                    // contained it. Checked *before* the removal below, and only where the set
                    // claims to be complete. See unmap.md.
                    if released && let Some(members) = pt.members() {
                        crate::obj::pagetables::membership::record_check(
                            members.contains(&arch.target),
                        );
                    }
                    pt.remove_invalidate(arch.target, cursor);
                    if pt.take_latch_notice() {
                        crate::obj::pagetables::invl_overflow::note_object(
                            slot.object().id(),
                            pt.invls_live(),
                            pt.invls_len(),
                        );
                    }
                    if released && slot.object().dec_map_count() == 0 {
                        slot.object().note_last_unmap();
                    }
                }
                unmapprofile::record(UStage::RemInv, t_ri);
            });
            unmap_census::record(n_arches, n_mapped, counted);
            // The map-count leak, caught in the act: a counted removal of a pending-delete
            // object that released nothing anywhere while the count is still positive means the
            // install's arch was neither visited nor already torn down with a dec -- the object
            // is now permanently unreapable (PD-STUCK mapcount, reclaim15). Names the members
            // set and visited count so the skipped arch is identifiable.
            if counted && slot.object().is_pending_delete() {
                let mc = slot.object().map_count();
                // Regions gone (this was the last), count still positive: the object is now
                // permanently unreapable. `n_mapped` says whether this removal released anything
                // (0 = the install's arch was already gone with no dec; >=1 = an arch beyond the
                // visited set still holds an entry).
                if mc > 0 && slot.object().mappings().len() <= 1 {
                    unmap_census::PD_STUCK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    static LEAK_LOGS: core::sync::atomic::AtomicUsize =
                        core::sync::atomic::AtomicUsize::new(0);
                    if LEAK_LOGS.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 64 {
                        log::warn!(
                            "unmap leaves stuck mapcount: obj {} mapcount {} released {} visited {} inc[map={} fault={}] charged-sctx {:x} members {:?}",
                            slot.object().id(),
                            mc,
                            n_mapped,
                            n_arches,
                            slot.object().inc_sites[0]
                                .load(core::sync::atomic::Ordering::Relaxed),
                            slot.object().inc_sites[1]
                                .load(core::sync::atomic::Ordering::Relaxed),
                            slot.object().inc_sctx.load(core::sync::atomic::Ordering::Relaxed),
                            members
                        );
                    }
                }
            }
            unmapprofile::record(UStage::Arches, t_arches);
            // Explicit so the shootdown wait in the guard's Drop is timed rather than folded into
            // whatever follows the block.
            let t = unmapprofile::start();
            drop(pt);
            unmapprofile::record(UStage::Shoot, t);
        }
        let t = unmapprofile::start();
        let t_sw = unmapprofile::start();
        guard.finish();
        unmapprofile::record(UStage::FinSwap, t_sw);

        // After the unmap, not before: syncing can block on the pager, and dirty state lives in the
        // object's own page tables, which unmapping a context's reference to them does not touch.
        if slot.should_sync.load(core::sync::atomic::Ordering::SeqCst) {
            if let Err(e) = slot.ctrl(MapControlCmd::Sync(core::ptr::null_mut()), 0) {
                log::error!("failed to sync object {}: {:?}", slot.object().id(), e);
            }
        }

        // An object marked for deletion while it was still mapped becomes reapable exactly here,
        // and nothing else notices: `ObjectControlCmd::Delete` checks only the object it marks,
        // and the idle loop's whole-map scan does not run at all while a cpu stays busy. Without
        // this, a create/map/delete/unmap loop retains every object it ever made -- measured as
        // `free=0` and memory exhaustion partway through the sysbench suite.
        //
        // Handed to the reaper rather than done here: reaping a pager-backed object issues a
        // delete to the userspace pager, and doing that inline on this path -- with syncs of the
        // same objects in flight -- wedged the contended-sync bench.
        if crate::obj::TARGETED_REAP && slot.object().is_pending_delete() {
            let t_rp = unmapprofile::start();
            crate::obj::request_reap(slot.object());
            unmapprofile::record(UStage::FinReap, t_rp);
        }
        unmapprofile::record(UStage::Finish, t);
        unmapprofile::record(UStage::Total, t_total);
        unmapprofile::record_hist(initiator, t_hist);
    }
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct VirtContextSlot {
    obj: ObjectRef,
    slot: Slot,
    prot: Protections,
    cache: CacheType,
    flags: MapFlags,
}

impl From<&VirtContextSlot> for ObjectContextInfo {
    fn from(info: &VirtContextSlot) -> Self {
        ObjectContextInfo::new(info.obj.clone(), info.prot, info.cache, info.flags)
    }
}

impl Drop for VirtContext {
    fn drop(&mut self) {
        // The registry holds a `Weak`, so this remove is bookkeeping, not lifetime: it keeps
        // dead entries from accumulating in the map. Sleeping-lock-safe by the same argument as
        // the rest of this destructor chain (region drops take object page-table mutexes).
        get_all_contexts().lock().remove(&self.id.value());
        // Settle the map-count accounting before the regions and arch contexts are discarded.
        // Making `ALL_CONTEXTS` weak let dead compartments' contexts actually drop -- but a bare
        // drop frees the arch page tables and the `RegionManager` without ever running
        // `dec_map_count` for the installs those arches held. Every object faulted only inside
        // the dying context kept a phantom count of exactly 1 and became permanently unreapable:
        // PD-STUCK measured ~7.3k objects / 2.2M pages (~8.4GB, one ~4MB heap span per dead
        // compartment) standing across many-reclaim12..17. `unregister_sctx` is the existing
        // teardown that walks each arch against each region, decs on release, hands newly
        // unmapped pending-delete objects to the reaper, and sweeps sctx-targeted regions -- run
        // it for every slot still registered. No concurrency to fear: the refcount is zero, so
        // no thread can be switching into or faulting through this context.
        let ids: Vec<ObjID> = self.secctx.lock().iter().map(|slot| slot.sctx).collect();
        for id in ids {
            self.unregister_sctx(id);
        }
    }
}

pub const HEAP_MAX_LEN: usize = 0x0000001000000000 / 16; //4GB

struct GlobalPageAlloc {
    alloc: linked_list_allocator::Heap,
    end: VirtAddr,
}

impl GlobalPageAlloc {
    fn extend(&mut self, len: usize, mapper: &VirtContext) {
        let cursor = MappingCursor::new(self.end, len);
        // TODO: wait-ok?
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        // Uninit, not zeroed: nothing reads kernel heap memory before writing it. The
        // page-table frames `fa` supplies below are a different matter and stay zeroed.
        let mut phys = UninitPageProvider::new(FrameAllocFlags::KERNEL, settings);
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );

        mapper.with_arch(KERNEL_SCTX, |arch| {
            arch.map(cursor, &mut phys, &mut fa);
        });
        self.end = self.end.offset(len).unwrap();
        // Safety: the extension is backed by memory that is directly after the previous call to
        // extend.
        unsafe {
            self.alloc.extend(len);
        }
    }

    fn init(&mut self, mapper: &VirtContext) {
        let len = 2 * 1024 * 1024;
        let cursor = MappingCursor::new(self.end, len);
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::WriteBack,
            MappingFlags::GLOBAL,
        );
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        // Uninit, not zeroed: nothing reads kernel heap memory before writing it. The
        // page-table frames `fa` supplies below are a different matter and stay zeroed.
        let mut phys = UninitPageProvider::new(FrameAllocFlags::KERNEL, settings);

        mapper.with_arch(KERNEL_SCTX, |arch| {
            arch.map(cursor, &mut phys, &mut fa);
        });
        self.end = self.end.offset(len).unwrap();
        // Safety: the initial is backed by memory.
        unsafe {
            self.alloc.init(VirtAddr::HEAP_START.as_mut_ptr(), len);
        }
    }
}

// Safety: the internal heap contains raw pointers, which are not Send. However, the heap is
// globally mapped and static for the lifetime of the kernel.
unsafe impl Send for GlobalPageAlloc {}

static GLOBAL_PAGE_ALLOC: Spinlock<GlobalPageAlloc> = Spinlock::new(GlobalPageAlloc {
    alloc: linked_list_allocator::Heap::empty(),
    end: VirtAddr::HEAP_START,
});

impl KernelMemoryContext for VirtContext {
    fn allocate_chunk(&self, layout: core::alloc::Layout) -> Result<NonNull<u8>, TwzError> {
        heapprofile::record_alloc(layout.size());
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        let res = glb.alloc.allocate_first_fit(layout);
        match res {
            Err(_) => {
                let size = layout
                    .pad_to_align()
                    .size()
                    .next_multiple_of(Table::level_to_page_size(Table::last_level()))
                    * 2;
                let start = crate::instant::Instant::now();
                glb.extend(size, self);
                heapprofile::record_grow(size, start);
                glb.alloc
                    .allocate_first_fit(layout)
                    .map_err(|_| ResourceError::OutOfMemory.into())
            }
            Ok(x) => Ok(x),
        }
    }

    unsafe fn deallocate_chunk(&self, layout: core::alloc::Layout, ptr: NonNull<u8>) {
        heapprofile::record_free();
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        unsafe {
            glb.alloc.deallocate(ptr, layout);
        }
    }

    fn init_allocator(&self) {
        let mut glb = GLOBAL_PAGE_ALLOC.lock();
        glb.init(self);
    }

    fn prep_smp(&self) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        self.with_arch(KERNEL_SCTX, |arch| {
            arch.unmap(
                MappingCursor::new(
                    VirtAddr::start_user_memory(),
                    VirtAddr::end_user_memory() - VirtAddr::start_user_memory(),
                ),
                &mut fa,
            )
        });
    }

    type Handle<T> = KernelObjectVirtHandle<T>;

    fn insert_kernel_object<T>(&self, info: ObjectContextInfo) -> Self::Handle<T> {
        // Ahead of the lock: see `precharge_slot_map`.
        let mut fa = take_or_new_frame_allocator();
        Self::precharge_slot_map(&mut fa);

        let mut kernel_slots_counter = kernel_slot_counter().lock();
        let slot = kernel_slots_counter
            .kernel_slots_nums
            .pop()
            .unwrap_or_else(|| {
                let cur = kernel_slots_counter.cur_kernel_slot;
                kernel_slots_counter.cur_kernel_slot += 1;
                let max = Slot::try_from(
                    VirtAddr::end_kernel_object_memory()
                        .offset(-1isize)
                        .unwrap(),
                )
                .unwrap()
                .raw();
                if cur > max {
                    panic!("out of kernel object slots");
                }
                Slot(cur)
            });
        let (_is_ok, default_prot) = info.object().check_id();
        let new_slot_info = MapRegion {
            object: info.object().clone(),
            range: slot.range(),
            offset: 0,
            prot: info.prot(),
            cache_type: info.cache(),
            flags: info.flags,
            target_sctx: info.target_sctx(),
            stable: None,
            default_prot,
            should_sync: AtomicBool::new(false),
            removed: AtomicBool::new(false),
        };
        // Slots come off a free list that is only pushed to once an unmap has fully finished (see
        // `KernelObjectVirtHandle::drop`), so this cannot collide with a teardown in progress.
        let guard = self
            .regions
            .begin_insert(slot)
            .expect("kernel object slot already occupied");
        // Same order as `insert_object`: registered before the install takes the map count.
        let region = Arc::new(new_slot_info);
        region.object().add_mapping(slot.raw(), &region);
        self.map_object(&region, &mut fa);
        guard.commit(region);
        KernelObjectVirtHandle {
            info,
            slot,
            _pd: PhantomData,
        }
    }
}

pub struct KernelObjectVirtHandle<T> {
    info: ObjectContextInfo,
    slot: Slot,
    _pd: PhantomData<T>,
}

impl<T> KernelObjectVirtHandle<T> {
    pub fn start_addr(&self) -> VirtAddr {
        VirtAddr::new(0)
            .unwrap()
            .offset(self.slot.raw() * MAX_SIZE)
            .unwrap()
    }

    pub fn id(&self) -> ObjID {
        self.info.object().id()
    }

    pub fn object(&self) -> &ObjectRef {
        self.info.object()
    }
}

impl<T> Drop for KernelObjectVirtHandle<T> {
    fn drop(&mut self) {
        crate::memory::context::kobjcensus::record(crate::memory::context::kobjcensus::Site::Drop);
        let kctx = kernel_context();
        // We don't need to tell the object that it's no longer mapped in the kernel context,
        // since object invalidation always informs the kernel context.
        let removal = kctx.regions.begin_remove(self.slot);
        if let Some((region, _)) = &removal {
            region.object().remove_mapping(self.slot.raw());
        }
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut pt = self.object().lock_page_tables();
        // Under the page tables, as in VirtContext::remove_object: a fault holding a clone of this
        // region must not re-map it behind the unmap below.
        if let Some((region, _)) = &removal {
            region
                .removed
                .store(true, core::sync::atomic::Ordering::SeqCst);
        }
        let obj_table = pt.context_table_addr();
        let released = kctx.with_arch(KERNEL_SCTX, |arch| {
            arch.unmap_object(
                MappingCursor::new(self.start_addr(), MAX_SIZE),
                obj_table,
                &mut fa,
            )
        });
        let last = released && self.object().dec_map_count() == 0;
        drop(pt);
        if last {
            self.object().note_last_unmap();
            // Hand the object to the reaper, as every other last-unmap site does. TARGETED_REAP
            // has no fallback scan, so an object whose *last* mapping was the kernel's KSO handle
            // (sctx objects, thread reprs) was stranded here: marked pending-delete, map count 0,
            // pages never freed -- and via ties, everything tied to it (a dead compartment's heap
            // spans) stayed undeletable too. Measured as PD-SPLIT "unmapped 7142 objs / 2.21M
            // pages" standing in many-reclaim12.
            if crate::obj::TARGETED_REAP && self.object().is_pending_delete() {
                crate::obj::request_reap(self.object());
            }
        }
        // Release the slot *before* publishing it to the free list. `insert_kernel_object` pops
        // from that list and claims the slot immediately, and a slot still marked as being torn
        // down would fail that claim.
        drop(removal);
        kernel_slot_counter()
            .lock()
            .kernel_slots_nums
            .push(self.slot);
    }
}

impl<T> KernelObjectHandle<T> for KernelObjectVirtHandle<T> {
    fn base(&self) -> &T {
        unsafe {
            self.start_addr()
                .offset(NULLPAGE_SIZE)
                .unwrap()
                .as_ptr::<T>()
                .as_ref()
                .unwrap()
        }
    }

    fn base_mut(&mut self) -> &mut T {
        unsafe {
            self.start_addr()
                .offset(NULLPAGE_SIZE)
                .unwrap()
                .as_mut_ptr::<T>()
                .as_mut()
                .unwrap()
        }
    }

    fn lea_raw<R>(&self, iptr: *const R) -> Option<&R> {
        let offset = iptr as usize;
        let size = size_of::<R>();
        if offset >= MAX_SIZE || offset.checked_add(size)? >= MAX_SIZE {
            return None;
        }
        unsafe {
            Some(
                self.start_addr()
                    .offset(offset)
                    .unwrap()
                    .as_ptr::<R>()
                    .as_ref()
                    .unwrap(),
            )
        }
    }

    fn lea_raw_mut<R>(&self, iptr: *mut R) -> Option<&mut R> {
        let offset = iptr as usize;
        let size = size_of::<R>();
        if offset >= MAX_SIZE || offset.checked_add(size)? >= MAX_SIZE {
            return None;
        }
        unsafe {
            Some(
                self.start_addr()
                    .offset(offset)
                    .unwrap()
                    .as_mut_ptr::<R>()
                    .as_mut()
                    .unwrap(),
            )
        }
    }
}

impl StableId for VirtContext {
    fn id(&self) -> &Id<'_> {
        &self.id
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct PageFaultFlags : u32 {
        const USER = 1;
        const INVALID = 2;
        const PRESENT = 4;
    }
}

/// How far `remove_object`'s per-security-context fan-out actually reaches.
///
/// `unmap_object` is 88% of all mapper-lock acquisitions because `remove_object` unmaps from
/// *every* attached security context while `map_object` installs into exactly one. Whether that is
/// waste depends on a distribution a mean cannot show: "objects live in ~13 contexts" and "most
/// live in one, a handful live in fifty" produce the same 88% and want opposite fixes -- a reverse
/// map on the object in the first case, and nothing at all in the second, where the fan-out would
/// be reaching contexts that genuinely hold the mapping.
///
/// `mapped` undercounts by design: a stable (privately-cloned) region never took a count against
/// the object, so its unmap cannot report having released one. Those are counted separately.
/// [`SlotMemo`] outcomes, printed at debug shutdown next to the other kernel profiles.
///
/// Hits and misses are counted at the same call site, one increment each, so the ratio has a
/// single denominator. A hit counter incremented only on the hit path reports ~100% by
/// construction and would say the same thing whether the design worked or not. `skips` counts the
/// calls the memo declined to answer at all (kernel object memory, or no current thread) and is
/// separate for the same reason: folded into misses it would understate the hit rate, folded into
/// hits it would flatter it.
///
/// Per the plan: check this before believing any timing result. A rate not near 100% means the
/// design is wrong, and no A/B will say so.
pub mod slotmemo {
    use core::sync::atomic::{AtomicU64, Ordering};

    /// A/B: keep the hit/miss counters below.
    ///
    /// They are a single global cache line written by every consultation. Routing the fault path
    /// through the memo took that from 72k consultations per boot to 2.8M, so on a machine where
    /// several cpus fault at once every one of these is a contended RMW on one line -- a cost paid
    /// by the workload for a diagnostic. Off, the counters read zero and [`print`] says nothing.
    pub const SLOTMEMO_STATS: bool = false;

    static HITS: AtomicU64 = AtomicU64::new(0);
    /// Cold miss with the memo full: the thread works more slots than [`super::SLOT_MEMO_LEN`], so
    /// this one would have been evicted even if it had been cached. Raising K addresses these.
    static COLD_CAPACITY: AtomicU64 = AtomicU64::new(0);
    /// Cold miss with room to spare: this slot was simply never cached by this thread. No cache
    /// size fixes these; they bound what the design can reach.
    static COLD_COMPULSORY: AtomicU64 = AtomicU64::new(0);
    /// Miss with an entry present that failed validation: its region was removed, or it belonged
    /// to another context.
    static INVALIDATED: AtomicU64 = AtomicU64::new(0);
    static SKIPS: AtomicU64 = AtomicU64::new(0);
    /// `regions` acquisitions taken by the batch resolver, and slots resolved under them.
    static LOCK_TAKEN: AtomicU64 = AtomicU64::new(0);
    static LOCK_RESOLVED: AtomicU64 = AtomicU64::new(0);

    pub fn record_hit() {
        if SLOTMEMO_STATS {
            HITS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_cold_capacity() {
        if SLOTMEMO_STATS {
            COLD_CAPACITY.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_cold_compulsory() {
        if SLOTMEMO_STATS {
            COLD_COMPULSORY.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_invalidated() {
        if SLOTMEMO_STATS {
            INVALIDATED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_skip() {
        if SLOTMEMO_STATS {
            SKIPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One `regions` acquisition that resolved `slots` of them.
    pub fn record_batch(slots: u64) {
        if !SLOTMEMO_STATS || slots == 0 {
            return;
        }
        LOCK_TAKEN.fetch_add(1, Ordering::Relaxed);
        LOCK_RESOLVED.fetch_add(slots, Ordering::Relaxed);
    }

    pub fn print() {
        let hits = HITS.load(Ordering::Relaxed);
        let capacity = COLD_CAPACITY.load(Ordering::Relaxed);
        let compulsory = COLD_COMPULSORY.load(Ordering::Relaxed);
        let cold = capacity + compulsory;
        let invalidated = INVALIDATED.load(Ordering::Relaxed);
        let misses = cold + invalidated;
        let skips = SKIPS.load(Ordering::Relaxed);
        let taken = LOCK_TAKEN.load(Ordering::Relaxed);
        let under = LOCK_RESOLVED.load(Ordering::Relaxed);
        if hits == 0 && misses == 0 && skips == 0 {
            return;
        }
        logln!(
            "== slot memo locks: {} regions acquisitions resolved {} slots, {} saved ({}%) ==",
            taken,
            under,
            under.saturating_sub(taken),
            if under == 0 {
                0
            } else {
                under.saturating_sub(taken) * 100 / under
            },
        );
        let looked = hits + misses;
        let total = looked + skips;
        // Denominators printed, not just the ratio: a 99% hit rate over 1% of the traffic reads as
        // success unless the share it was taken over is visible next to it.
        logln!(
            "== slot memo: {} hits, {} misses ({} cold = {} capacity + {} compulsory, {} invalidated) = {}% of {} consulted, {} skipped ({}% of {} calls) ==",
            hits,
            misses,
            cold,
            capacity,
            compulsory,
            invalidated,
            if looked == 0 { 0 } else { hits * 100 / looked },
            looked,
            skips,
            if total == 0 { 0 } else { skips * 100 / total },
            total,
        );
    }
}

pub mod unmap_census {
    use core::sync::atomic::{AtomicUsize, Ordering};

    /// Buckets: 0, 1, 2, 3, 4, 5-8, 9-16, 17+.
    const NR: usize = 8;
    static ARCHES: [AtomicUsize; NR] = [const { AtomicUsize::new(0) }; NR];
    static MAPPED: [AtomicUsize; NR] = [const { AtomicUsize::new(0) }; NR];
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static STABLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ARCH_VISITS: AtomicUsize = AtomicUsize::new(0);
    static ARCH_HITS: AtomicUsize = AtomicUsize::new(0);
    /// True maxima, because the buckets cannot give one: the top bucket is `17+` and unbounded, so
    /// an empty 9-16 bucket bounds the answer at `<= 8` rather than reporting it. Sizing a
    /// fixed-capacity membership structure off a bound inferred from an empty bucket is guessing --
    /// see unmap.md.
    static MAX_ARCHES: AtomicUsize = AtomicUsize::new(0);
    static MAX_MAPPED: AtomicUsize = AtomicUsize::new(0);
    /// Arch visits membership let us skip -- i.e. mapper-lock acquisitions not taken. Counted apart
    /// from `ARCH_VISITS` rather than by differencing two runs, so the win is legible within a
    /// single boot and does not depend on a baseline being comparable.
    static SKIPPED: AtomicUsize = AtomicUsize::new(0);

    pub fn record_skip() {
        SKIPPED.fetch_add(1, Ordering::Relaxed);
    }

    /// Objects left permanently unreapable by a removal: last region gone, `map_count` still
    /// positive because `for_each_arch_in` visited none of the membership set. The `log::warn!`
    /// at the detection site is capped at 8 lines, so that cap is a *cap* and not a count --
    /// this is the count. (Requested by twizzler-d3, whose two runs both saturated the cap.)
    pub static PD_STUCK: AtomicUsize = AtomicUsize::new(0);

    /// Detached an object-table entry whose owner could not be verified (`context_table_addr`
    /// was `None`); the dec was taken anyway. See `ArchContext::unmap_object`.
    static UNVERIFIED: AtomicUsize = AtomicUsize::new(0);
    /// Detached an entry that verifiably belonged to a different object; no dec.
    static FOREIGN: AtomicUsize = AtomicUsize::new(0);

    pub fn record_unverified() {
        UNVERIFIED.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_foreign() {
        FOREIGN.fetch_add(1, Ordering::Relaxed);
    }

    fn bucket(n: usize) -> usize {
        match n {
            0..=4 => n,
            5..=8 => 5,
            9..=16 => 6,
            _ => 7,
        }
    }

    pub fn record(arches: usize, mapped: usize, counted: bool) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        if !counted {
            STABLE_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        ARCH_VISITS.fetch_add(arches, Ordering::Relaxed);
        ARCH_HITS.fetch_add(mapped, Ordering::Relaxed);
        ARCHES[bucket(arches)].fetch_add(1, Ordering::Relaxed);
        MAPPED[bucket(mapped)].fetch_add(1, Ordering::Relaxed);
        // Guarded by a relaxed load: `fetch_max` has no native instruction on x86_64 or aarch64 and
        // lowers to a `lock cmpxchg` retry loop, so taking it unconditionally puts two contended
        // RMWs on every removal. The race is benign -- the maxima are monotonic and only read at
        // shutdown, so a lost update is re-attempted by the next caller to exceed it.
        if arches > MAX_ARCHES.load(Ordering::Relaxed) {
            MAX_ARCHES.fetch_max(arches, Ordering::Relaxed);
        }
        if mapped > MAX_MAPPED.load(Ordering::Relaxed) {
            MAX_MAPPED.fetch_max(mapped, Ordering::Relaxed);
        }
    }

    pub fn print() {
        let calls = CALLS.load(Ordering::Relaxed);
        if calls == 0 {
            emerglogln!("== unmap census: none");
            return;
        }
        let visits = ARCH_VISITS.load(Ordering::Relaxed);
        let hits = ARCH_HITS.load(Ordering::Relaxed);
        let g = |a: &[AtomicUsize; NR]| {
            let mut v = [0usize; NR];
            for (i, x) in v.iter_mut().enumerate() {
                *x = a[i].load(Ordering::Relaxed);
            }
            v
        };
        let (ab, mb) = (g(&ARCHES), g(&MAPPED));
        emerglogln!(
            "== unmap census: {} removals ({} stable), {} arch visits ({}/100 mean), {} held a mapping ({}%), {} skipped, max {} arches, max {} mapped",
            calls,
            STABLE_CALLS.load(Ordering::Relaxed),
            visits,
            visits * 100 / calls,
            hits,
            if visits == 0 { 0 } else { hits * 100 / visits },
            SKIPPED.load(Ordering::Relaxed),
            MAX_ARCHES.load(Ordering::Relaxed),
            MAX_MAPPED.load(Ordering::Relaxed),
        );
        emerglogln!(
            "== unmap census releases: {} unverified (dec taken), {} foreign (dec withheld)",
            UNVERIFIED.load(Ordering::Relaxed),
            FOREIGN.load(Ordering::Relaxed)
        );
        emerglogln!(
            "== unmap census pd-stuck: {} objects left unreapable (log capped at 8)",
            PD_STUCK.load(Ordering::Relaxed)
        );
        emerglogln!(
            "== unmap census arches/removal [0,1,2,3,4,5-8,9-16,17+]: {} {} {} {} {} {} {} {}",
            ab[0],
            ab[1],
            ab[2],
            ab[3],
            ab[4],
            ab[5],
            ab[6],
            ab[7]
        );
        emerglogln!(
            "== unmap census mapped/removal  [0,1,2,3,4,5-8,9-16,17+]: {} {} {} {} {} {} {} {}",
            mb[0],
            mb[1],
            mb[2],
            mb[3],
            mb[4],
            mb[5],
            mb[6],
            mb[7]
        );
    }
}
