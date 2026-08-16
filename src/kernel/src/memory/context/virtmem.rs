//! This mod implements [UserContext] and [KernelMemoryContext] for virtual memory systems.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::{
    marker::PhantomData,
    mem::size_of,
    ops::Range,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

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
    obj::{ObjectRef, PageNumber, PtGuard, pagetables::ObjectPageTable},
    once::Once,
    processor::{mp::current_processor, tls_ready},
    security::KERNEL_SCTX,
    spinlock::Spinlock,
    thread::current_thread_ref,
};

pub mod fault;
pub mod region;
mod tests;

pub use fault::page_fault;

/// A type that implements [super::Context] for virtual memory systems.
pub struct VirtContext {
    secctx: Mutex<BTreeMap<ObjID, ArchContext>>,
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
    target_cache: Spinlock<BTreeMap<ObjID, ArchContextTarget>>,
    regions: Mutex<RegionManager>,
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

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static CHECKID: AtomicU64 = AtomicU64::new(0);
    static TOTAL: AtomicU64 = AtomicU64::new(0);

    pub fn record_checkid(ns: u64) {
        CHECKID.fetch_add(ns, Ordering::Relaxed);
    }

    /// Charges the whole of `insert_object` on drop, so an early return is counted too.
    pub struct Timer(pub Instant);

    impl Drop for Timer {
        fn drop(&mut self) {
            COUNT.fetch_add(1, Ordering::Relaxed);
            TOTAL.fetch_add(
                (Instant::now() - self.0).as_nanos() as u64,
                Ordering::Relaxed,
            );
        }
    }

    pub fn print() {
        let n = COUNT.load(Ordering::Relaxed);
        if n == 0 {
            return;
        }
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
                return Some(e.region.object.clone());
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

static ALL_CONTEXTS: Once<Mutex<BTreeMap<u64, Arc<VirtContext>>>> = Once::new();

fn get_all_contexts() -> &'static Mutex<BTreeMap<u64, Arc<VirtContext>>> {
    ALL_CONTEXTS.call_once(|| Mutex::new(BTreeMap::new()))
}

pub fn with_each_context(cb: impl FnMut(&Arc<VirtContext>)) {
    let all = get_all_contexts();
    let contexts = {
        let contexts = all.lock();
        contexts.values().cloned().collect::<Vec<_>>()
    };
    contexts.iter().for_each(cb);
}

impl VirtContext {
    fn __new(kernel_arch: Option<ArchContext>) -> Self {
        let mut secctx = Mutex::new(BTreeMap::new());
        // We ensure that the BTree never changes while we hold the lock.
        secctx.set_safe_with_spinlocks(true);
        let new = Self {
            regions: Mutex::new(RegionManager::default()),
            memo_tag: MEMO_TAGS.fetch_add(1, Ordering::Relaxed),
            is_kernel: kernel_arch.is_some(),
            id: CONTEXT_IDS.next(),
            secctx,
            kernel_arch,
            target_cache: Spinlock::new(BTreeMap::new()),
        };
        new
    }

    /// Construct a new context for the kernel.
    pub fn new_kernel() -> Arc<Self> {
        let this = Arc::new(Self::__new(Some(ArchContext::new_kernel())));
        let target = this.arch().target;
        // Built outside the spinlock and swapped in, for the reason `register_sctx` gives: filling
        // the map allocates, and `target_cache` is a spinlock.
        let mut targets = BTreeMap::new();
        targets.insert(KERNEL_SCTX, target);
        core::mem::swap(&mut *this.target_cache.lock(), &mut targets);
        // Cache the root now, while we're safely outside the thread-switch path.
        KERNEL_ARCH_TARGET.call_once(|| target);
        let all = get_all_contexts();
        all.lock().insert(this.id.value(), this.clone());
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
        all.lock().insert(this.id.value(), this.clone());
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

    /// Run `cb` against every arch context this context owns: the kernel's single one, or a user
    /// context's one per attached security context.
    fn for_each_arch(&self, mut cb: impl FnMut(&ArchContext)) {
        if let Some(arch) = self.kernel_arch.as_ref() {
            cb(arch);
            return;
        }
        for arch in self.secctx.lock().values() {
            cb(arch);
        }
    }

    pub fn try_with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> Option<R> {
        if let Some(arch) = self.single_arch(sctx) {
            return Some(cb(arch));
        }
        let secctx = self.secctx.lock();
        secctx.get(&sctx).map(|arch| cb(arch))
    }

    pub fn with_arch<R>(&self, sctx: ObjID, cb: impl FnOnce(&ArchContext) -> R) -> R {
        if let Some(arch) = self.single_arch(sctx) {
            return cb(arch);
        }
        let secctx = self.secctx.lock();
        cb(secctx
            .get(&sctx)
            .expect("cannot get arch mapper for unattached security context"))
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
        // Reading the thread's own `secctx.active()` instead of `get_sctx(active_id())` is faster
        // (68% of this function). The two used to differ -- `get_sctx(0)` returned `Err` and
        // skipped this whole block -- but both now resolve to the single `kernel_sctx()`, so the
        // swap is available if this shows up in a profile again. See pagerperf.md 17.
        if let Ok(sctx) = crate::security::get_sctx(sctx) {
            let perms = sctx.lookup(info.object().id(), info.default_prot);
            let mut pt = if info.stable.is_some() {
                PtGuard::new(info.stable.as_ref().unwrap())
            } else {
                info.object.lock_page_tables()
            };
            self.try_with_arch(sctx.id(), |arch| {
                pt.add_invalidate(arch.target, cursor);
                let settings = MappingSettings::new(
                    perms.effective(info.default_prot, info.prot),
                    info.cache_type,
                    MappingFlags::USER,
                );
                arch.object_map(cursor, &mut *pt, settings, fa);
            });
        };
    }

    pub fn ensure_object_mapped(
        &self,
        sctxid: ObjID,
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
            arch.ensure_object_mapped(cursor, object_tables, settings, &mut fa)
        })
    }

    pub fn print_objects(&self) {
        let mut slots = self.regions.lock();
        for obj in slots.objects().copied().collect::<Vec<_>>().iter() {
            log!("{} => ", obj);
            for mapping in slots.object_mappings(*obj) {
                log!("{:?}, ", mapping.range);
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
        let mut secctx = self.secctx.lock();
        if secctx.contains_key(&sctx) {
            return;
        }
        secctx.insert(sctx, arch);
        // Rebuild the target cache. We have to do it this way because we cannot allocate
        // memory while holding the target_cache lock (as it's a spinlock).
        let mut new_target_cache = BTreeMap::new();
        for value in secctx.iter() {
            new_target_cache.insert(*value.0, value.1.target);
        }
        // Swap out the target caches, dropping the old one after the spinlock is released.
        {
            let mut target_cache = self.target_cache.lock();
            core::mem::swap(&mut *target_cache, &mut new_target_cache);
        }
    }

    pub fn unregister_sctx(&self, sctx: ObjID) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut secctx = self.secctx.lock();
        if !secctx.contains_key(&sctx) {
            return;
        }
        let arch = secctx.remove(&sctx);

        // Retire the target *before* `arch` is dropped at the end of this function, not after: the
        // drop frees the root page table and releases the PCID, and until the cache is rebuilt a
        // concurrent `switch_to` on this sctx would still find the old target and load it. Doing it
        // here shrinks that window from "the whole unmap walk below" to "a switch that already read
        // the target", and keeps a recycled PCID from being installed against a freed root -- which
        // would alias one address space's translations onto whoever gets the PCID next. Same
        // build-then-swap dance as register_sctx, because target_cache is a spinlock and the
        // allocation has to happen outside it.
        let mut new_target_cache = BTreeMap::new();
        for value in secctx.iter() {
            new_target_cache.insert(*value.0, value.1.target);
        }
        {
            let mut target_cache = self.target_cache.lock();
            core::mem::swap(&mut *target_cache, &mut new_target_cache);
        }
        drop(secctx);
        drop(new_target_cache);

        if let Some(arch) = arch {
            let regions = self.regions.lock();
            let capacity = regions.mappings().count();
            drop(regions);

            let mut region_list = alloc::vec::Vec::<Arc<MapRegion>>::with_capacity(capacity);
            let regions = self.regions.lock();
            for region in regions.mappings() {
                if region.range.start.raw() == 0 {
                    continue;
                }
                region_list.push(region.clone());
            }
            drop(regions);

            for region in region_list {
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
                if counted && released {
                    pt.dec_map_count();
                    if pt.map_count() == 0 {
                        region.object().note_last_unmap();
                    }
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
    pub fn lookup_object_refs_cached(&self, slots: &[Slot], out: &mut [Option<ObjectRef>]) {
        assert_eq!(slots.len(), out.len());
        out.fill(None);

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

        // One `regions` acquisition for everything that missed.
        let mut resolved: [Option<Arc<MapRegion>>; RESOLVE_CHUNK] = [const { None }; RESOLVE_CHUNK];
        {
            let regions = self.regions.lock();
            let mut under_lock = 0;
            for (i, slot) in slots.iter().enumerate() {
                if out[i].is_some() || reroute(slot) {
                    continue;
                }
                under_lock += 1;
                resolved[i] = regions.lookup_region(slot.start_vaddr()).cloned();
            }
            // Realized, not hypothetical: `sync batching`'s saveable count is computed at syscall
            // entry and reports the same number whether this function batches or not. This counts
            // acquisitions actually taken against slots actually resolved under them, so the
            // difference is the saving that happened.
            slotmemo::record_batch(under_lock);
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

    pub fn lookup_slot(&self, slot: usize) -> Option<Arc<MapRegion>> {
        let slot = &Slot::try_from(slot).ok()?;
        self.regions
            .lock()
            .lookup_region(slot.start_vaddr())
            .cloned()
    }

    /// Fill `buf` with the numbers of the slots that have something mapped in them, ascending,
    /// skipping the first `offset`. Returns how many were written; short of `buf.len()` means the
    /// enumeration is done. Backs `sys_enumerate_slots`.
    pub fn enumerate_slots(&self, buf: &mut [u64], offset: usize) -> Result<usize, TwzError> {
        let mut slots = self
            .regions
            .lock()
            .mappings()
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
        self.target_cache.lock().get(&sctx).copied()
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
        let tc = self.target_cache.lock();
        let target = tc
            .get(&sctx)
            .expect("tried to switch to a non-registered sctx");
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
        let _guard = mapprofile::Timer(crate::instant::Instant::now());
        log::debug!(
            "insert {} to {:?} {:?}",
            object_info.object.id(),
            slot.start_vaddr(),
            object_info.prot(),
        );

        let mut stable = None;
        if object_info.flags.contains(MapFlags::STABLE) {
            stable = Some(Arc::new(Mutex::new(
                object_info.object().cow_clone_page_tables()?,
            )));
        }

        let t_check = crate::instant::Instant::now();
        let (_is_ok, default_prot) = object_info.object.check_id();
        mapprofile::record_checkid((crate::instant::Instant::now() - t_check).as_nanos() as u64);
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
            should_sync: Arc::new(AtomicBool::new(false)),
            removed: Arc::new(AtomicBool::new(false)),
        };

        // Ahead of the lock: see `precharge_slot_map`.
        let mut fa = take_or_new_frame_allocator();
        Self::precharge_slot_map(&mut fa);

        // Check the slot is free before mapping, and hold the lock across the map: otherwise a
        // racing insert can clobber our object table entry, and a Busy return leaves behind a
        // mapping plus the map count taken for it, which keeps the object from ever being reaped.
        // Lock order (regions -> object page tables -> secctx) matches insert_kernel_object.
        let mut slots = self.regions.lock();
        if slots.lookup_region(slot.start_vaddr()).is_some() {
            return Err(ResourceError::Busy.into());
        }
        self.map_object(&new_slot_info, &mut fa);
        slots.insert_region(new_slot_info);
        Ok(())
    }

    fn lookup_object(&self, info: Self::MappingInfo) -> Option<ObjectContextInfo> {
        if info.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            kernel_context().lookup_object(info)
        } else {
            let slots = self.regions.lock();
            slots
                .lookup_region(info.start_vaddr())
                .map(|info| (&**info).into())
        }
    }

    fn lookup_object_ref(&self, info: Self::MappingInfo) -> Option<ObjectRef> {
        if info.start_vaddr().is_kernel_object_memory() && !self.is_kernel {
            kernel_context().lookup_object_ref(info)
        } else {
            let slots = self.regions.lock();
            slots
                .lookup_region(info.start_vaddr())
                .map(|region| region.object().clone())
        }
    }

    fn remove_object(&self, info: Self::MappingInfo) {
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut slots = self.regions.lock();
        let Some(slot) = slots.remove_region(info.start_vaddr()) else {
            return;
        };
        fault::note_unmap(info.raw(), slot.object());

        // Tear the mapping down while still holding the regions lock: insert_object claims a free
        // slot under it and maps immediately (see there), so releasing it here would let another
        // object be mapped into this slot and then have its entry removed by the unmap below.
        // Lock order (regions -> object page tables -> secctx) matches insert_object.
        {
            // Whichever page tables the fault path would use for this region -- taking the same
            // one is what makes the `removed` store below and that path's check of it ordered.
            let mut pt = if let Some(stable) = slot.stable.as_ref() {
                PtGuard::new(stable)
            } else {
                slot.object().lock_page_tables()
            };
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
            let members: Option<heapless::Vec<ArchContextTarget, 32>> = (counted)
                .then(|| pt.members().map(|m| m.iter().copied().collect()))
                .flatten();
            self.for_each_arch(|arch| {
                if let Some(members) = members.as_ref()
                    && !members.contains(&arch.target)
                {
                    unmap_census::record_skip();
                    return;
                }
                let cursor = slot.mapping_cursor(0, MAX_SIZE);
                let released = arch.unmap_object(cursor, obj_table, &mut fa);
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
                    if released {
                        pt.dec_map_count();
                        if pt.map_count() == 0 {
                            slot.object().note_last_unmap();
                        }
                    }
                }
            });
            unmap_census::record(n_arches, n_mapped, counted);
        }
        drop(slots);

        // After the unmap, not before: syncing can block on the pager, and dirty state lives in the
        // object's own page tables, which unmapping a context's reference to them does not touch.
        if slot.should_sync.load(core::sync::atomic::Ordering::SeqCst) {
            if let Err(e) = slot.ctrl(MapControlCmd::Sync(core::ptr::null_mut()), 0) {
                log::error!("failed to sync object {}: {:?}", slot.object().id(), e);
            }
        }
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
        // TODO: remove appropriate invalidations from objects.
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

        let mut slots = self.regions.lock();
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
            should_sync: Arc::new(AtomicBool::new(false)),
            removed: Arc::new(AtomicBool::new(false)),
        };
        self.map_object(&new_slot_info, &mut fa);
        slots.insert_region(new_slot_info);
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
        let kctx = kernel_context();
        let region = {
            let mut slots = kctx.regions.lock();
            // We don't need to tell the object that it's no longer mapped in the kernel context,
            // since object invalidation always informs the kernel context.
            slots.remove_region(self.slot.start_vaddr())
        };
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::KERNEL | FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let mut pt = self.object().lock_page_tables();
        // Under the page tables, as in VirtContext::remove_object: a fault holding a clone of this
        // region must not re-map it behind the unmap below.
        if let Some(region) = &region {
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
        let last = released && {
            pt.dec_map_count();
            pt.map_count() == 0
        };
        drop(pt);
        if last {
            self.object().note_last_unmap();
        }
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
        HITS.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cold_capacity() {
        COLD_CAPACITY.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cold_compulsory() {
        COLD_COMPULSORY.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_invalidated() {
        INVALIDATED.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_skip() {
        SKIPS.fetch_add(1, Ordering::Relaxed);
    }

    /// One `regions` acquisition that resolved `slots` of them.
    pub fn record_batch(slots: u64) {
        if slots == 0 {
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
