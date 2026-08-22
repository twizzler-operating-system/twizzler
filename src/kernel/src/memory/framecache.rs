//! Per-cpu frame caching, magazine/depot form.
//!
//! Replacement for the `TLS_FRAME_ALLOCATOR` pool in [`crate::memory::tracker`]. The design and
//! the measurements behind it are in `framecache.md`; what follows is only what a reader of this
//! file needs.
//!
//! # Shape
//!
//! Frames move in **magazines** -- fixed arrays of [`MAG_SIZE`] frames, allocated once at [`init`]
//! and recycled forever. Each cpu holds at most two (one clean, one dirty) in thread-local
//! storage; everything else lives in one global [`Depot`]. A cpu that empties its magazine swaps
//! it for a full one from the depot, and a cpu that fills one pushes it there. So the depot -- the
//! only shared structure -- is touched once per `MAG_SIZE` frames in each direction, and the
//! common case is an array index behind a `cli`.
//!
//! # Why a depot rather than stealing from a peer
//!
//! The producer and the consumer of freed frames are different cpus by design: object teardown is
//! deferred to a reaper thread (`obj::defer_teardown`), so the thread that allocates is never the
//! thread that frees. Measured, that costs ~780x more global-allocator refills at smp4 than at
//! smp1 where the two share a cpu.
//!
//! The reaper is **one** thread, though, so a design that has to *find* the donor finds it with
//! probability `1/(N-1)` per attempt and gets worse as cpus are added -- backwards. A depot does
//! not search: the producer's surplus is already there. It also needs no cross-cpu mutation, which
//! is what makes the interrupt-safety argument below a local one.
//!
//! # Rules this file is written to
//!
//! - **Never allocates after [`init`].** Every path here is reachable from `free_frame`, and a free
//!   that allocates can re-enter the allocator it is freeing for -- `GlobalPageAlloc::extend`
//!   builds a frame allocator while `allocate_chunk` holds `GLOBAL_PAGE_ALLOC`, and growing
//!   anything there self-deadlocks a non-reentrant spinlock. The magazine count is fixed and the
//!   depot's stacks are arrays, so this is a property of the types rather than of a comment.
//!   Running out of magazines is not a failure: it falls through to the physical allocator, which
//!   is the correct thing to do when the cache is saturated.
//! - **The depot lock is a leaf.** Never held across the PFA lock or `GLOBAL_PAGE_ALLOC`. Callers
//!   that free to the PFA take magazines out under the lock, drop it, and free outside. The
//!   ordering matters because trim runs from a context that may already hold allocator state.
//! - **Cleanliness is which magazine a frame is in**, never `PhysicalFrameFlags::ZEROED`. That bit
//!   is cleared at every hand-out (`finish_raw_alloc`), so it means "in the allocator's custody and
//!   known zero", not "this page is zero". Tracking by list also makes "how much needs zeroing" an
//!   O(1) read instead of a scan.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::frame::FrameRef;
use crate::{
    processor::tls_ready,
    spinlock::{LockGuard, SpinLoop, Spinlock},
};

/// Master gate. `false` leaves every hook below inert and the old `TLS_FRAME_ALLOCATOR` pool in
/// sole charge, which is the arm every measurement to date was taken against.
///
/// One const rather than the pool's eight, deliberately. Those grew one per experiment and the
/// result is that no two of them were ever varied independently in the same boot, so the tree
/// carries a combination nobody has measured. This ships as a single A/B and stays that way until
/// there is a reason to split it that comes with a measurement.
pub const ENABLED: bool = true;

/// Frames per magazine, and therefore the amortization factor: one depot acquisition per this
/// many frames, in each direction.
///
/// 64 to start, matching the old pool's `POOL_REFILL_BATCH` so the alloc-side lock rate is
/// unchanged and only the free side moves. The number that should decide it is depot acquisitions
/// per frame, not wall clock. Larger amortizes better and makes the background zeroer's unit
/// longer: at 64 a magazine is 256 KB of memset, already at the edge of what should happen
/// between reschedule checks.
pub const MAG_SIZE: usize = 64;

/// Magazines in the system, fixed at [`init`].
///
/// This is the entire memory bound, and unlike the pool it replaces it is an explicit constant
/// rather than a side effect of whatever the hot path last reserved. At 64 frames each, 40
/// magazines is 10 MB across the whole machine -- against the old pool's 8 MB *per cpu*, which it
/// reached by accident and could not give back.
const MAX_MAGAZINES: usize = 64;

/// Empty magazines the free path may not consume, held back for the background zeroer.
///
/// **Measured, not guessed.** Without it the first armed boot reached a steady state of
/// `clean=0 dirty=37 empty=0`: every magazine in the system full of unzeroed frames. The zeroer
/// needs an empty magazine to drain a dirty one *into*, so at that point it returned `false`
/// forever and 93.8% of hand-outs paid an inline 4 KiB memset -- against the old pool's 99.86%,
/// i.e. the clean/dirty split bought almost nothing because it never got to run.
///
/// The free path is the right thing to squeeze. A free that cannot get a magazine goes to the
/// physical allocator, which is the pressure valve working as designed; a zeroer that cannot get
/// one silently stops being a feature. Four is enough for continuous progress and is 10% of the
/// magazines.
const ZERO_RESERVE: usize = 4;

/// A batch of frames in transit between a cpu and the depot.
///
/// Not `Copy`, not `Clone`, and only ever moved by pointer: a magazine is a place, and there is
/// exactly one of each.
pub struct Magazine {
    frames: heapless::Vec<FrameRef, MAG_SIZE>,
}

impl Magazine {
    const fn new() -> Self {
        Self {
            frames: heapless::Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.frames.len() == MAG_SIZE
    }

    /// Returns the frame back to the caller if there was no room, so nothing can be dropped on the
    /// floor by a caller that forgot to check [`Self::is_full`].
    fn push(&mut self, frame: FrameRef) -> Option<FrameRef> {
        self.frames.push(frame).err()
    }

    fn pop(&mut self) -> Option<FrameRef> {
        self.frames.pop()
    }
}

/// A stack of magazines.
///
/// An array rather than a `Vec` so that a push can never allocate: every magazine is in exactly
/// one place at a time, so `MAX_MAGAZINES` slots is enough for all three stacks to hold everything
/// at once, and a full push is unreachable rather than merely unlikely. It is still handled, and
/// handled by giving the magazine back to the caller.
struct MagStack {
    mags: [Option<&'static mut Magazine>; MAX_MAGAZINES],
    len: usize,
}

impl MagStack {
    const fn new() -> Self {
        Self {
            mags: [const { None }; MAX_MAGAZINES],
            len: 0,
        }
    }

    /// Panics rather than returning the magazine on overflow, and that is the right trade here.
    ///
    /// Overflow is unreachable by pigeonhole -- there are exactly `MAX_MAGAZINES` magazines, each
    /// is in exactly one place, and each stack has that many slots -- so this can only fire if the
    /// conservation invariant has already been broken somewhere else. The alternative, handing the
    /// magazine back for every caller to deal with, produced a `let _ = push(..)` at seven call
    /// sites, and each of those silently *loses* a magazine, permanently: the symptom is a cache
    /// that gets slower across a boot with nothing in any counter to say why. A panic names the
    /// bug where it happens. Live in release -- `[profile.release]` sets only `debug = true`, so a
    /// `debug_assert` here would be dead in exactly the arm the long soaks run.
    fn push(&mut self, mag: &'static mut Magazine) {
        assert!(
            self.len < MAX_MAGAZINES,
            "framecache: magazine stack overflow -- conservation invariant broken"
        );
        self.mags[self.len] = Some(mag);
        self.len += 1;
    }

    fn pop(&mut self) -> Option<&'static mut Magazine> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        self.mags[self.len].take()
    }

    fn len(&self) -> usize {
        self.len
    }
}

/// The one shared structure. Touched once per [`MAG_SIZE`] frames.
///
/// A plain spinlock, deliberately: magazines are recycled and never freed, which is exactly the
/// case a lock-free stack gets wrong (ABA), and the fix for that costs more review than it can
/// possibly save at one acquisition per 64 frames. "As lock-free as possible" is a statement about
/// rate, not about algorithm -- and the rate is what [`stat::DEPOT_ACQ`] measures. Revisit only
/// with that number in hand.
struct Depot {
    /// Full, and every frame in them is zeroed.
    clean: MagStack,
    /// Full, contents undefined.
    dirty: MagStack,
    /// The recycling pool. A free that finds this empty falls through to the PFA.
    empty: MagStack,
}

impl Depot {
    const fn new() -> Self {
        Self {
            clean: MagStack::new(),
            dirty: MagStack::new(),
            empty: MagStack::new(),
        }
    }
}

static DEPOT: Spinlock<Depot> = Spinlock::new(Depot::new());

/// Depth of [`Depot::clean`], readable without the depot lock.
///
/// The free path consults this on **every** free to decide whether to zero, and taking the depot
/// lock there would undo the entire point of a per-cpu cache. Maintained only through
/// [`Depot::push_clean`] / [`Depot::pop_clean`] so it cannot drift from the stack it mirrors --
/// a gauge updated at scattered call sites is how the old pool's accounting became unreadable.
static CLEAN_MAGS: AtomicUsize = AtomicUsize::new(0);

impl Depot {
    fn push_clean(&mut self, mag: &'static mut Magazine) {
        self.clean.push(mag);
        CLEAN_MAGS.store(self.clean.len(), Ordering::Relaxed);
    }

    fn pop_clean(&mut self) -> Option<&'static mut Magazine> {
        let m = self.clean.pop();
        CLEAN_MAGS.store(self.clean.len(), Ordering::Relaxed);
        m
    }
}

/// Set once [`init`] has run. Read before every depot access, because the paths below are live
/// from early boot -- `GlobalPageAlloc::init` maps the kernel heap before anything here exists.
static READY: AtomicBool = AtomicBool::new(false);

/// Frames resident in the cache, across both the depot and every cpu's magazines.
///
/// The correction term for leak fitting, not a leak signal: a cached frame stays `ALLOCATED` and
/// stays charged to its class, so it depresses `idle`/`free_pages` and inflates `kernel_used` or
/// `page_data` exactly as a leaked one does. Exported as `trk.pooled`.
///
/// Maintained per **magazine transfer** where possible rather than per frame -- the point of
/// magazines is that the shared-cacheline traffic scales with `1/MAG_SIZE`, and a per-frame gauge
/// update would put all of it straight back.
static CACHED_FRAMES: AtomicUsize = AtomicUsize::new(0);

pub fn cached_frames() -> usize {
    CACHED_FRAMES.load(Ordering::Relaxed)
}

macro_rules! counters {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {
        $($(#[$m])* pub static $name: AtomicU64 = AtomicU64::new(0);)*
        pub const NAMES: &[&str] = &[$(stringify!($name)),*];
        pub const NR: usize = NAMES.len();
        pub fn snapshot() -> [u64; NR] {
            [$($name.load(Ordering::Relaxed)),*]
        }
    };
}

/// Counters, differenced by [`crate::perfmark`].
///
/// Chosen so every falsifier in `framecache.md` Part 4 is a counter rather than a wall-clock
/// number: `object_create_delete_nomap`'s identical-arm drift is 12.9%, which is larger than most
/// of the effects here, so nanoseconds cannot settle them.
pub mod stat {
    use core::sync::atomic::{AtomicU64, Ordering};

    counters!(
        /// Frames served from a cpu's own magazine without touching the depot. The hit path.
        LOCAL_HIT,
        /// Frames served after swapping an empty magazine for a full one.
        DEPOT_HIT,
        /// Draws that found neither, and fell through to the physical allocator. Together with
        /// `LOCAL_HIT` + `DEPOT_HIT` this is every draw, so the ratio is readable without a
        /// separate total.
        MISS,
        /// Depot acquisitions. **This is the amortization measurement**: divided into
        /// `LOCAL_HIT + DEPOT_HIT` it should read ~`1/MAG_SIZE`. If it reads ~1, magazines are
        /// thrashing at the boundary and want the two-magazine rule before anything else is
        /// concluded from any other number here.
        DEPOT_ACQ,
        /// Frames put into a cpu's own magazine on the free path.
        FREE_LOCAL,
        /// Frames zeroed *on the free path*, so they entered the cache already clean. Divided into
        /// `FREE_LOCAL` this is how often the demand cap let the free path pay; against
        /// `ZEROED_INLINE` it is how much of the total memset moved off the allocating thread.
        FREE_ZEROED,
        /// Frees that could not be cached and went to the physical allocator: no empty magazine
        /// was available, or the cache is off, or memory is tight. Split from `FREE_NO_MAG`
        /// because "the valve is doing its job under pressure" and "the cache is undersized" want
        /// opposite responses.
        FREE_TO_PFA,
        /// Subset of the above: specifically no empty magazine. This is the one that sizes
        /// `MAX_MAGAZINES`.
        FREE_NO_MAG,
        /// Hand-outs that had to memset 4 KiB because a zeroed frame was asked for and only a
        /// dirty one was available. Falls toward zero as the background zeroer keeps up; the
        /// old pool ran at 99.86%.
        ZEROED_INLINE,
        /// Hand-outs to a caller that did **not** ask for zeroes and was therefore served from
        /// the dirty side. Every one of these is a 4 KiB memset that used to happen and no longer
        /// does *and* a clean frame left for someone who needed it -- `Frame::cow_frame` and
        /// `UninitPageProvider` are the two consumers.
        SERVED_DIRTY,
        /// Frees that declined to zero, split by which of `should_zero_on_free`'s two caps
        /// refused. One counter could not tell them apart and they want opposite fixes:
        /// `FREE_ZERO_STOCKED` is the demand cap doing its job and says raise
        /// [`FREE_ZERO_WATER`]; `FREE_ZERO_INTS` is a free reached from inside somebody's
        /// spinlock, which no constant here can change.
        FREE_ZERO_STOCKED,
        FREE_ZERO_INTS,
        /// Frees the caller declared already-zero: leftover precharge that was never handed to
        /// anyone. These enter the clean side without a memset, which is the whole point.
        FREE_KNOWN_ZERO,
        /// Magazines converted dirty -> clean by the background zeroer, and frames zeroed by it.
        BG_MAGS,
        BG_FRAMES,
        /// Times the zeroer was **entered** and found work, versus turned away.
        ///
        /// Added after two wrong diagnoses in a row, both of which inferred the call rate from
        /// `BG_MAGS` instead of measuring it. `BG_MAGS / BG_CALLS` is the batch actually achieved:
        /// ~1 means the loop breaks after one magazine (the dirty stack is empty *at the moment
        /// the zeroer runs*, whatever a later sample says), ~`ZERO_BATCH_MAGS` means batching
        /// works and the limit is cpu or wakeups. Those two want opposite fixes and no counter
        /// present could tell them apart.
        BG_CALLS,
        /// Entered with no empty magazine to drain into. If this is large the zeroer is starved by
        /// `ZERO_RESERVE` being too small, not by scheduling.
        BG_NO_SPARE,
        /// Entered with a spare but no dirty magazine to convert. Large means the zeroer is
        /// keeping up and the demand is elsewhere -- which would refute the whole diagnosis.
        BG_NO_DIRTY,
        /// Frames returned to the physical allocator by the pressure trim.
        TRIMMED,
    );
}

/// Preallocate every magazine. Called once, from a context that may allocate.
///
/// After this returns, nothing in this module reaches the kernel heap again for the life of the
/// boot. That is the whole of R3: the free path cannot allocate because there is no allocation
/// left to make, rather than because each caller was audited.
pub fn init() {
    if !ENABLED {
        return;
    }
    let mut depot = DEPOT.lock();
    for _ in 0..MAX_MAGAZINES {
        // Leaked deliberately: a magazine outlives every borrow of it and is never freed, so an
        // owning handle would only be a `Drop` that must never run.
        let mag: &'static mut Magazine =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(Magazine::new()));
        depot.empty.push(mag);
    }
    drop(depot);
    READY.store(true, Ordering::Release);
    logln!(
        "[framecache] {} magazines of {} frames ({} KB capacity)",
        MAX_MAGAZINES,
        MAG_SIZE,
        MAX_MAGAZINES * MAG_SIZE * 4
    );
}

fn ready() -> bool {
    READY.load(Ordering::Relaxed)
}

/// Take the depot lock, counting the acquisition.
///
/// Counted here and nowhere else so that [`stat::DEPOT_ACQ`] cannot drift from the thing it
/// claims to measure by someone adding a call site.
fn depot() -> LockGuard<'static, Depot, SpinLoop> {
    stat::DEPOT_ACQ.fetch_add(1, Ordering::Relaxed);
    DEPOT.lock()
}

// ---------------------------------------------------------------------------------------------
// Per-cpu half
// ---------------------------------------------------------------------------------------------

/// This cpu's two magazines.
///
/// Two rather than one because a `ZEROED` request and an unzeroed one must not have to share:
/// serving `cow_frame` out of the clean side would consume a zeroed page in order to overwrite it
/// immediately, which is the exact waste [`stat::SERVED_DIRTY`] exists to count going away.
struct Cache {
    clean: Option<&'static mut Magazine>,
    dirty: Option<&'static mut Magazine>,
}

/// Per-cpu, owning cpu only, touched with interrupts disabled, and addressed through [`cache`] so
/// the address is derived from the segment base *at the point of use*.
///
/// **There is no lock, and adding a cross-cpu accessor would need one.** The depot exists so that
/// no such accessor is needed: rebalancing, background zeroing and pressure trim all operate on
/// the depot, never on another cpu's magazines. The most a cpu's magazines can hold is
/// `2 * MAG_SIZE` frames, which is why leaving them unreachable costs nothing.
#[thread_local]
static mut TLS_CACHE: Cache = Cache {
    clean: None,
    dirty: None,
};

/// Offset of [`TLS_CACHE`] from the thread pointer. One ELF TLS template, so one layout, identical
/// on every cpu. Zero means "not computed"; no TLS variable sits at the thread pointer.
static TLS_CACHE_TPOFF: AtomicUsize = AtomicUsize::new(0);

#[cold]
fn init_tls_cache_tpoff() -> usize {
    // Interrupts off because the two halves -- the variable's address and this cpu's thread
    // pointer -- must come from the *same* cpu, which is the property [`cache`] exists to keep.
    let int = crate::interrupt::disable();
    let off =
        (core::ptr::addr_of!(TLS_CACHE) as usize).wrapping_sub(crate::arch::processor::tls_base());
    // Zero is the "not computed yet" sentinel, so a variable that genuinely sat at the thread
    // pointer would recompute on every access *and* resolve to the thread pointer itself -- the
    // wrong address, silently, for the life of the boot. Unreachable in practice (x86_64 variant
    // II puts TLS below the base, so this subtraction wraps to something enormous), which is
    // exactly why it earns an assert rather than a comment: an unreachable case that is silently
    // wrong when reached is the kind nothing ever catches.
    assert_ne!(off, 0, "framecache: TLS_CACHE sits at the thread pointer");
    TLS_CACHE_TPOFF.store(off, Ordering::Relaxed);
    crate::interrupt::set(int);
    off
}

/// This cpu's cache, addressed so the address cannot be stale.
///
/// The obvious `&mut TLS_CACHE` is **not** safe here, and this is not a theoretical worry -- it is
/// the defect (tag `fadf-audit`) found in the frame cache that preceded the pool this file
/// replaces. Taking the address of a `#[thread_local]` lets the compiler materialise
/// `thread_pointer + offset` into a general register; it hoisted that computation *above* the
/// `cli`, spilled it, and reloaded it inside the critical section. A general register survives
/// migration and the thread pointer does not, so a thread preempted in that window resumes on
/// another cpu and mutates the *previous* cpu's cache while correctly interrupts-off, which is two
/// cpus in one structure.
///
/// Reading the segment base here, under the caller's interrupts-off region, closes it: plain
/// `asm!` is volatile and cannot be reordered across the interrupt-disable asm, so the base is
/// this cpu's for as long as interrupts stay off. Same shape as `tracker::tls_fa` and
/// `thread::read_current_thread_ptr`, and copied from them rather than re-derived.
///
/// On aarch64 there is no segment override to lean on and this settles for materialising the
/// address inside the region, carrying the same residual risk those two do. If aarch64 becomes a
/// target for this, move `Cache` onto `Processor` behind a spinlock -- there a hoisted address is
/// merely the wrong cpu's cache, which is benign, because frames are fungible.
///
/// # Safety
/// Caller must hold interrupts disabled for the whole borrow.
#[allow(static_mut_refs)]
#[inline(always)]
unsafe fn cache() -> &'static mut Cache {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let mut off = TLS_CACHE_TPOFF.load(Ordering::Relaxed);
        if core::intrinsics::unlikely(off == 0) {
            off = init_tls_cache_tpoff();
        }
        let base: usize;
        core::arch::asm!(
            "mov {b}, fs:[0]",
            b = lateout(reg) base,
            options(nostack, preserves_flags),
        );
        &mut *(base.wrapping_add(off) as *mut Cache)
    }
    #[cfg(not(target_arch = "x86_64"))]
    unsafe {
        &mut *core::ptr::addr_of_mut!(TLS_CACHE)
    }
}

/// Run `f` on this cpu's cache with interrupts disabled.
///
/// Every access goes through here so the safety condition on [`cache`] is discharged in one place.
/// `f` must not allocate, must not take the depot lock, and must be short: it runs with interrupts
/// off. The depot swaps below deliberately close this region before taking the depot lock, which
/// is the leaf-lock rule stated at the top of the file.
#[inline(always)]
fn with_cache<T>(f: impl FnOnce(&mut Cache) -> T) -> T {
    crate::interrupt::with_disabled(|| unsafe { f(cache()) })
}

/// Which side of the cache a request is about. Not a bool at the call sites: `alloc(true)` reads
/// as "yes allocate" and the argument is the opposite of the interesting one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Want {
    /// The caller will read the page before writing it. Page tables, zero-fill faults.
    Zeroed,
    /// The caller overwrites the page immediately: `Frame::cow_frame`, `UninitPageProvider`.
    /// Served from the dirty side by preference, which skips a memset *and* leaves a clean frame
    /// for a caller that needs one.
    Any,
}

// ---------------------------------------------------------------------------------------------
// Alloc path
// ---------------------------------------------------------------------------------------------

/// Take one frame from the cache, or `None` if it cannot be served without the physical allocator.
///
/// The frame comes back **raw**: still marked `POOLED`, still charged to whatever class its last
/// owner had, and -- if `want` is [`Want::Any`], or if a zeroed request had to fall back --
/// possibly dirty. The caller finishes it. That split is deliberate and load-bearing: finishing
/// can memset 4 KiB, and this returns with interrupts already re-enabled so that the memset does
/// not happen inside a critical section.
///
/// Returns `(frame, needs_zeroing)`. `needs_zeroing` is true only when the caller asked for
/// [`Want::Zeroed`] and got a frame off the dirty side.
pub fn alloc_one(want: Want) -> Option<(FrameRef, bool)> {
    if !ENABLED || !ready() || !tls_ready() {
        return None;
    }
    // Fast path: this cpu's own magazine. No lock, no depot, one bounds check.
    if let Some(r) = with_cache(|c| take_local(c, want)) {
        stat::LOCAL_HIT.fetch_add(1, Ordering::Relaxed);
        CACHED_FRAMES.fetch_sub(1, Ordering::Relaxed);
        return Some(r);
    }
    // Slow path: swap the empty magazine for a full one and retry once. Exactly once -- a loop
    // here could spin against other cpus draining the depot, and the fallthrough to the physical
    // allocator is always correct.
    if !refill(want) {
        stat::MISS.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    match with_cache(|c| take_local(c, want)) {
        Some(r) => {
            stat::DEPOT_HIT.fetch_add(1, Ordering::Relaxed);
            CACHED_FRAMES.fetch_sub(1, Ordering::Relaxed);
            Some(r)
        }
        None => {
            // Another cpu emptied what we just installed, or `refill` installed the other side.
            stat::MISS.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

/// Most frames one [`alloc_many`] call will return, whatever it is asked for.
///
/// Exists so a caller can track the per-frame `needs_zeroing` answers in a fixed-width word. That
/// sounds like the caller's problem until it is not: the first version of the hook in the tracker
/// used a `u64` bitmask and `min(63)`, which silently folded every flag past the 64th onto one
/// bit -- and the failure mode of a *lost* `needs_zeroing` is a dirty frame handed to page-table
/// code as zeroed. Capping here makes that unrepresentable instead of asking each caller to
/// remember.
pub const MAX_BATCH: usize = 64;

/// Draw up to `want_n` frames -- at most [`MAX_BATCH`] -- handing each to `push`. Returns how many
/// were accepted.
///
/// `push` receives the same raw `(frame, needs_zeroing)` pair [`alloc_one`] returns and answers
/// whether it took it; a `false` stops the draw with the frame still in the cache. A closure
/// rather than a container because the caller's capacity is the caller's business -- the one in
/// the tracker must not grow its store, and handing it a `Vec` to fill would put the allocation
/// this whole file exists to avoid back on the free path's neighbour.
///
/// **`push` runs with interrupts disabled.** It must not allocate, must not lock, and must be
/// short. It must also not zero: the `needs_zeroing` frames are handed back unzeroed precisely so
/// the caller can memset them after this returns.
///
/// Bounded to one refill rather than looping until `want_n` is satisfied, for the same reason
/// [`alloc_one`] retries exactly once. A short return is not a failure; the caller's remaining
/// need falls through to the physical allocator, which is where a large request belongs anyway.
pub fn alloc_many(
    want: Want,
    want_n: usize,
    mut push: impl FnMut(FrameRef, bool) -> bool,
) -> usize {
    if !ENABLED || want_n == 0 || !ready() || !tls_ready() {
        return 0;
    }
    let want_n = want_n.min(MAX_BATCH);
    // One interrupts-off region per round, not one per frame: that is the difference between
    // amortizing the `cli`/`sti` pair and paying it `MAG_SIZE` times.
    let drain = |push: &mut dyn FnMut(FrameRef, bool) -> bool, room: usize| {
        with_cache(|c| {
            let mut n = 0;
            while n < room {
                let Some((frame, nz)) = take_local(c, want) else {
                    break;
                };
                if !push(frame, nz) {
                    // Refused. Put it back rather than dropping it -- it is still `POOLED` and
                    // still charged, so losing it here would be a real leak.
                    put_local(c, frame, nz, want);
                    break;
                }
                n += 1;
            }
            n
        })
    };

    let local = drain(&mut push, want_n);
    if local > 0 {
        stat::LOCAL_HIT.fetch_add(local as u64, Ordering::Relaxed);
    }
    let mut got = local;
    if got < want_n && refill(want) {
        let after = drain(&mut push, want_n - got);
        if after > 0 {
            stat::DEPOT_HIT.fetch_add(after as u64, Ordering::Relaxed);
        }
        got += after;
    }
    if got > 0 {
        CACHED_FRAMES.fetch_sub(got, Ordering::Relaxed);
    } else {
        stat::MISS.fetch_add(1, Ordering::Relaxed);
    }
    got
}

/// One frame out of this cpu's magazines, honouring `want`'s preference and falling back to the
/// other side rather than returning nothing.
///
/// The fallback is what makes the clean/dirty split a *preference* rather than a partition: a
/// zeroed request served off the dirty side pays a memset, which is the old pool's behaviour and
/// therefore the floor, never a regression.
fn take_local(c: &mut Cache, want: Want) -> Option<(FrameRef, bool)> {
    let (first, second) = match want {
        Want::Zeroed => (&mut c.clean, &mut c.dirty),
        Want::Any => (&mut c.dirty, &mut c.clean),
    };
    if let Some(mag) = first.as_deref_mut() {
        if let Some(frame) = mag.pop() {
            if want == Want::Any {
                stat::SERVED_DIRTY.fetch_add(1, Ordering::Relaxed);
            }
            return Some((frame, false));
        }
    }
    if let Some(mag) = second.as_deref_mut() {
        if let Some(frame) = mag.pop() {
            // Only one of the two fallbacks costs anything: `Zeroed` off the dirty side memsets,
            // `Any` off the clean side just spends a zeroed page on someone who did not need it.
            let needs_zeroing = want == Want::Zeroed;
            if needs_zeroing {
                stat::ZEROED_INLINE.fetch_add(1, Ordering::Relaxed);
            }
            return Some((frame, needs_zeroing));
        }
    }
    None
}

/// Swap this cpu's empty magazine of the preferred kind for a full one from the depot.
///
/// Returns whether anything was installed. Structured as take-empty-out, then depot, then
/// put-full-in, so that the depot lock is never held while interrupts are disabled for the cache
/// and the cache is never borrowed while the depot lock is held. Those are two separate rules and
/// this is the only function that has to satisfy both.
fn refill(want: Want) -> bool {
    // Step 1, interrupts off: lift out whichever magazine is empty, so the depot exchange below
    // has something to give back. Leaves the cache with `None` on that side for the duration --
    // which is *recoverable*, unlike the old pool's `None`, because any other draw on this cpu
    // simply falls through to the depot itself.
    let spent = with_cache(|c| {
        let (first, second) = match want {
            Want::Zeroed => (&mut c.clean, &mut c.dirty),
            Want::Any => (&mut c.dirty, &mut c.clean),
        };
        if first.as_deref().is_none_or(Magazine::is_empty) {
            first.take()
        } else if second.as_deref().is_none_or(Magazine::is_empty) {
            second.take()
        } else {
            // Neither side is empty, so the caller's `take_local` should not have failed. Racing
            // with an interrupt-context free is the only way here; nothing to do.
            None
        }
    });

    // Step 2, depot lock, no cache borrow held.
    let mut d = depot();
    // Preference order mirrors `take_local`: a `Zeroed` request would rather have a clean magazine
    // and memset nothing, and an `Any` request would rather leave the clean ones alone.
    let (full, filled_clean) = match want {
        Want::Zeroed => match d.pop_clean() {
            Some(m) => (Some(m), true),
            None => (d.dirty.pop(), false),
        },
        Want::Any => match d.dirty.pop() {
            Some(m) => (Some(m), false),
            None => (d.pop_clean(), true),
        },
    };
    let Some(full) = full else {
        // Nothing to install. Put the empty magazine back where it can be reused by the free path
        // rather than holding it hostage on a cpu that has no frames anyway.
        if let Some(spent) = spent {
            d.empty.push(spent);
        }
        return false;
    };
    if let Some(spent) = spent {
        d.empty.push(spent);
    }
    drop(d);

    // Step 3, interrupts off again: install it.
    //
    // The slot can have filled underneath us, since the depot lock above was taken with the cache
    // unborrowed. On this cpu that needs an interrupt-context free; across cpus it cannot happen
    // at all. Either way frames are now available on that side, so both arms return `true` -- what
    // differs is only which stack the magazine we are holding belongs back on.
    let displaced = with_cache(|c| {
        let slot = if filled_clean {
            &mut c.clean
        } else {
            &mut c.dirty
        };
        if slot.as_deref().is_some_and(|m| !m.is_empty()) {
            Displaced::TheFullOne(full)
        } else {
            match core::mem::replace(slot, Some(full)) {
                Some(empty) => Displaced::AnEmptyOne(empty),
                None => Displaced::Nothing,
            }
        }
    });
    match displaced {
        Displaced::Nothing => {}
        // Routed by the magazine's own state rather than by which branch produced it. The two
        // agree today -- an `AnEmptyOne` really is empty, because step 1 only lifts out empty
        // ones -- but that is an argument spanning three functions, and if it ever stops holding,
        // an `AnEmptyOne` carrying frames would put them on the empty stack, which loses them
        // silently. Asking the magazine costs one load.
        Displaced::AnEmptyOne(mag) | Displaced::TheFullOne(mag) => {
            let mut d = depot();
            if mag.is_empty() {
                d.empty.push(mag);
            } else if filled_clean {
                d.push_clean(mag);
            } else {
                d.dirty.push(mag);
            }
        }
    }
    true
}

/// What [`refill`]'s install step ended up holding. Named cases rather than a nested `Option`
/// because *which* one happened is the interesting part; where the magazine goes is decided by
/// inspecting it, above.
enum Displaced {
    Nothing,
    AnEmptyOne(&'static mut Magazine),
    TheFullOne(&'static mut Magazine),
}

/// Put a frame back where [`take_local`] would have found it. Only for a caller that took one and
/// could not use it -- it does **not** touch [`CACHED_FRAMES`], because the matching decrement has
/// not happened yet either.
fn put_local(c: &mut Cache, frame: FrameRef, was_dirty: bool, want: Want) {
    // `was_dirty` is the `needs_zeroing` flag the take produced, which is only meaningful for a
    // `Zeroed` request; an `Any` request never reports one, so its frame goes back to whichever
    // side it came from, which for `Any` is dirty-first.
    let clean_side = match want {
        Want::Zeroed => !was_dirty,
        Want::Any => false,
    };
    let slot = if clean_side {
        &mut c.clean
    } else {
        &mut c.dirty
    };
    if let Some(mag) = slot.as_deref_mut() {
        if mag.push(frame).is_none() {
            return;
        }
    }
    // No magazine on that side, or it filled. The other side is still a correct home -- a clean
    // frame in the dirty magazine only costs a redundant memset later, never correctness. Frames
    // are fungible; magazines are only a hint about cleanliness.
    let other = if clean_side {
        &mut c.dirty
    } else {
        &mut c.clean
    };
    if let Some(mag) = other.as_deref_mut() {
        if mag.push(frame).is_none() {
            return;
        }
    }
    // Nowhere to put it. This is why `alloc_many`'s `push` contract says "refuse" rather than
    // "take and sort out later": there is no allocation available to make room, so a frame that
    // cannot go back has to be handed to the physical allocator by the caller instead. Reached
    // only if both magazines filled between the take and the put, which needs an interrupt.
    log::warn!("framecache: nowhere to return a frame, handing it to the pfa");
    if frame.clear_pooled() {
        CACHED_FRAMES.fetch_sub(1, Ordering::Relaxed);
    }
    super::tracker::free_frame_nopark(frame);
}

// ---------------------------------------------------------------------------------------------
// Free path
// ---------------------------------------------------------------------------------------------

/// Clean magazines at which free-path zeroing switches off. **This is the cap.**
///
/// Zeroing on the free path is not free work relocated -- it is the same memset, moved from
/// whoever allocates the frame to whoever frees it. That is worth doing here because the two are
/// different threads by design (object teardown is deferred to a reaper), so on a multi-cpu box it
/// parallelises instead of queueing; and because it sidesteps the race that defeated the
/// background zeroer entirely -- a frame zeroed at free *enters* the cache clean, so allocations
/// cannot take it before the zeroer does.
///
/// But it is still work on a path that must stay cheap, so it stops as soon as it is not needed:
/// once the depot holds this many clean magazines, frees go back to being free. Demand-driven
/// rather than rate-limited, because a fixed rate would keep paying when the supply is already
/// stocked and stop paying when it is not -- exactly backwards.
///
/// 8 magazines is 512 frames, ~2 MB of standing clean supply.
const FREE_ZERO_WATER: usize = 24;

/// Check that a `known_zero` free really is zero, and panic if not.
///
/// This is the only unsafe-in-practice claim in the cache: a frame accepted as clean is handed
/// to page-table code that parses it as entries, so a wrong hint is silent corruption rather
/// than a crash. `debug_assert` is useless here -- `[profile.release]` sets only `debug = true`,
/// so it compiles out of exactly the build the benches run. Leave this **on** until an armed
/// boot has passed millions of frames through it, then turn it off; 4 KiB of compares is far
/// too expensive to keep on the free path permanently.
const VERIFY_KNOWN_ZERO: bool = false;

/// Whether this free should pay for a 4 KiB memset.
///
/// Two independent caps, and the second is a safety property rather than a tuning choice:
///
/// 1. **Demand.** Stop once the clean supply is stocked ([`FREE_ZERO_WATER`]). One relaxed load.
/// 2. **Context.** Only with interrupts enabled. `free_frame` is reachable from inside other
///    subsystems' spinlock sections -- `GlobalPageAlloc::extend` runs under `GLOBAL_PAGE_ALLOC`,
///    and every `Spinlock::lock` in this kernel disables interrupts -- and a 370 ns memset there
///    lengthens somebody else's critical section, not ours. `interrupt::get()` is the cheapest
///    honest proxy for "am I inside someone's lock": it cannot be fooled the way
///    `Thread::is_critical` can, because spinlocks here deliberately do *not* mark the thread
///    critical (see the TODO at `Spinlock::lock`).
#[inline]
fn should_zero_on_free() -> bool {
    if !crate::interrupt::get() {
        stat::FREE_ZERO_INTS.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    if CLEAN_MAGS.load(Ordering::Relaxed) >= FREE_ZERO_WATER {
        stat::FREE_ZERO_STOCKED.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    true
}

/// Panic unless every byte of `frame` is zero.
///
/// Read as `u64`s rather than bytes so the compare is 512 loads instead of 4096, and reported
/// with the offset of the first non-zero word -- "somewhere in this frame" is not enough to find
/// which caller lied.
fn verify_zero(frame: FrameRef) {
    let ptr = frame.start_address().kernel_vaddr().as_ptr::<u64>();
    let words = frame.size() / core::mem::size_of::<u64>();
    for i in 0..words {
        // Safety: the frame is mapped in the kernel's physical map and owned by this free.
        let v = unsafe { core::ptr::read_volatile(ptr.add(i)) };
        assert!(
            v == 0,
            "known_zero free is not zero: frame {:?} word {} = {:#x}",
            frame,
            i,
            v
        );
    }
}

/// Offer a freed level-0 frame to this cpu's cache. Returns whether it was taken.
///
/// A refusal is not an error and the caller must free the frame to the physical allocator as it
/// otherwise would. Refusing is in fact the *point* under pressure: the cache is a buffer, and a
/// buffer that never gives memory back is the defect this design replaces.
///
/// The frame must arrive already marked `POOLED` and with its `COW` bit cleared -- the caller owns
/// that, because the caller is also the one that must assert against a double free.
pub fn free_one(frame: FrameRef) -> bool {
    free_one_hinted(frame, false)
}

/// [`free_one`], where the caller can guarantee the frame's contents are already all zero.
///
/// `known_zero` is a promise that nothing has written this frame since it was last zeroed. The
/// only caller that passes `true` is `FrameAllocator`'s drop, and only for frames left in its
/// `precharge` list: those were never popped by `try_allocate`, so they were never handed to a
/// consumer. See [`VERIFY_KNOWN_ZERO`] for the check that keeps that honest.
pub fn free_one_hinted(frame: FrameRef, known_zero: bool) -> bool {
    if !ENABLED || !ready() || !tls_ready() {
        return false;
    }
    // A freed frame's contents are whatever its last owner left, so it always goes to the dirty
    // side regardless of what `PhysicalFrameFlags::ZEROED` currently says. Trusting that bit here
    // is how a dirty page reaches page-table code that parses it as entries.
    // Zero *before* choosing a side, so the frame enters the cache already clean and no allocation
    // can take it ahead of a zeroer. `zero()` sets `PhysicalFrameFlags::ZEROED` and this undoes it
    // for the same reason the background zeroer does: cleanliness here is which magazine a frame
    // is in, and a flag left set survives hand-out, survives being dirtied, and then tells
    // `raw_free_frame` to file a dirty page on the physical allocator's zeroed free list.
    let clean = if known_zero {
        if VERIFY_KNOWN_ZERO {
            verify_zero(frame);
        }
        stat::FREE_KNOWN_ZERO.fetch_add(1, Ordering::Relaxed);
        true
    } else if should_zero_on_free() {
        frame.zero();
        frame.set_not_zero();
        stat::FREE_ZEROED.fetch_add(1, Ordering::Relaxed);
        true
    } else {
        false
    };
    if with_cache(|c| push_side(c, frame, clean)) {
        stat::FREE_LOCAL.fetch_add(1, Ordering::Relaxed);
        CACHED_FRAMES.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    // The local magazine on that side is full. Push it to the depot and take an empty one.
    if !flush_side(clean) {
        stat::FREE_NO_MAG.fetch_add(1, Ordering::Relaxed);
        stat::FREE_TO_PFA.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    if with_cache(|c| push_side(c, frame, clean)) {
        stat::FREE_LOCAL.fetch_add(1, Ordering::Relaxed);
        CACHED_FRAMES.fetch_add(1, Ordering::Relaxed);
        return true;
    }
    stat::FREE_TO_PFA.fetch_add(1, Ordering::Relaxed);
    false
}

fn push_side(c: &mut Cache, frame: FrameRef, clean: bool) -> bool {
    let slot = if clean { &mut c.clean } else { &mut c.dirty };
    match slot.as_deref_mut() {
        Some(mag) => mag.push(frame).is_none(),
        None => false,
    }
}

/// Hand this cpu's full dirty magazine to the depot and install an empty one.
///
/// Same three-step shape as [`refill`] and for the same two reasons: the depot lock is a leaf, and
/// the cache must not be borrowed across it.
fn flush_side(clean: bool) -> bool {
    let full = with_cache(|c| {
        let slot = if clean { &mut c.clean } else { &mut c.dirty };
        match slot.as_deref() {
            Some(mag) if mag.is_full() => slot.take(),
            Some(_) => None,
            // No magazine at all yet: nothing to flush, but the exchange below still needs to run
            // so this cpu gets its first one.
            None => None,
        }
    });
    let mut d = depot();
    // Leave [`ZERO_RESERVE`] behind: see the const. The zeroer's supply of empty magazines has to
    // be protected from the free path, or the cache saturates into all-dirty and the clean side
    // never exists.
    let empty = if d.empty.len() > ZERO_RESERVE {
        d.empty.pop()
    } else {
        None
    };
    match (full, empty) {
        (Some(full), Some(empty)) => {
            if clean {
                d.push_clean(full);
            } else {
                d.dirty.push(full);
            }
            drop(d);
            install_side(empty, clean)
        }
        (Some(full), None) => {
            // No empty to swap in. Put the full one back and let the caller free to the physical
            // allocator -- this is the pressure valve, and reaching it means the cache is at its
            // bound, which is the state in which it *should* stop absorbing frees.
            if clean {
                d.push_clean(full);
            } else {
                d.dirty.push(full);
            }
            false
        }
        (None, Some(empty)) => {
            drop(d);
            install_side(empty, clean)
        }
        (None, None) => false,
    }
}

/// Install `mag` as this cpu's magazine on the given side, returning whether it now has room.
fn install_side(mag: &'static mut Magazine, clean: bool) -> bool {
    let displaced = with_cache(|c| {
        let slot = if clean { &mut c.clean } else { &mut c.dirty };
        match slot.as_deref() {
            // Someone installed one underneath us and it has room; keep theirs.
            Some(existing) if !existing.is_full() => Some(mag),
            _ => core::mem::replace(slot, Some(mag)),
        }
    });
    if let Some(mag) = displaced {
        let mut d = depot();
        // Empty-or-not, not full-or-not: a partially filled magazine still holds frames, so the
        // empty stack would lose them. The dirty stack's only contract is "not known to be zero",
        // which a partial magazine satisfies; what it costs is one short refill later.
        if mag.is_empty() {
            d.empty.push(mag);
        } else if clean {
            d.push_clean(mag);
        } else {
            d.dirty.push(mag);
        }
    }
    true
}

// ---------------------------------------------------------------------------------------------
// Background zeroing
// ---------------------------------------------------------------------------------------------

/// Convert one dirty magazine to a clean one. Returns whether it did any work.
///
/// Called from `background_worker`, the existing `Priority::BACKGROUND` kernel thread that already
/// does this job for the physical allocator. **Not the idle loop**, which was the first design:
/// an idle-loop zeroer only runs on a cpu that is idle, and the workloads that make the cache miss
/// are exactly the ones where no cpu is. A schedulable thread runs on whatever cpu has slack.
///
/// Preemptible mid-magazine. 64 frames is 256 KB of memset, 20-60 us, which is too long to hold
/// against a reschedule -- so the partial goes back to the dirty stack. Frames are fungible, so
/// re-zeroing the part already done costs time and nothing else.
/// Magazines one [`background_zero_one`] call will convert before yielding.
///
/// **This constant is the whole of the zeroing fix, and it was measured rather than chosen.**
///
/// The first armed boot converted exactly one magazine per call and then yielded, which supplied
/// 378k zeroed frames against 2.04M demanded -- so 93.8% of hand-outs paid an inline 4 KiB memset
/// and the clean side of the cache was empty at every sample. The throttle was never memset
/// bandwidth and never the per-frame reschedule check (the counters read 64.00 frames per
/// completed magazine, so no magazine was ever cut short). It was **work per scheduling slot**:
/// this thread runs at `BACKGROUND`, so slots are the scarce resource, and one magazine is 256 KB
/// of them.
///
/// The comparison that makes the number obvious is `frame::background_zero_iter`, which does the
/// same job for the physical allocator in the same loop on the same thread, and keeps up. It
/// collects up to four regions of 4 MB and its frames include 2 MB huge pages, so a single
/// `frame.zero()` there can cover 512 pages: up to ~16 MB per slot against this path's 256 KB,
/// about 64x. That is the entire difference, and it is why the baseline arm served 54% of its
/// frames pre-zeroed while this one served 18%.
///
/// 16 magazines is 4 MB per call, matching one of `background_zero_iter`'s regions, and 16x the
/// supply against a measured 5.4x shortfall. Deliberately **not** interruptible between magazines:
/// checking `needs_reschedule` there would return to one-per-call on exactly the busy machine this
/// exists to serve, and the precedent for an uninterrupted batch is `background_zero_iter` running
/// up to 16 MB with no check at all once it has collected.
const ZERO_BATCH_MAGS: usize = 16;

/// Convert dirty magazines to clean ones, up to [`ZERO_BATCH_MAGS`] of them. Returns whether it
/// did any work.
///
/// Called from `background_worker`, the existing `Priority::BACKGROUND` kernel thread that already
/// does this job for the physical allocator. **Not the idle loop**, which was the first design: an
/// idle-loop zeroer only runs on a cpu that is idle, and the workloads that make the cache miss
/// are exactly the ones where no cpu is.
pub fn background_zero_one() -> bool {
    if !ready() {
        return false;
    }
    // **Two magazines, not one.** The obvious single-magazine version -- pop, zero, push back --
    // returns the zeroed frames to the same end it pops from, so the next pass re-zeroes exactly
    // what the last one did and never reaches the untouched tail. Draining one magazine into
    // another makes progress monotone with no cursor to keep: whatever ends up in `into` is
    // zeroed, whatever is left in `from` is not, and both are valid magazines at every point.
    //
    // Only **one** spare is needed for the whole batch, not one per magazine: a drained `from` is
    // itself empty, so it becomes the next iteration's `into`. Carrying it across iterations also
    // keeps the depot lock off the per-magazine path, which matters because failed and redundant
    // acquisitions are what currently hold `frames/acq` at 16-20 against a target of 64.
    let Some(mut spare) = ({
        let mut d = depot();
        match d.empty.pop() {
            Some(m) => Some(m),
            None => {
                // Every magazine holds frames. Nothing to zero into, and this is also the state in
                // which zeroing matters least -- the cache is saturated and the free path is
                // already spilling to the physical allocator.
                None
            }
        }
    }) else {
        stat::BG_NO_SPARE.fetch_add(1, Ordering::Relaxed);
        return false;
    };

    let mut zeroed = 0u64;
    let mut mags = 0u64;
    for _ in 0..ZERO_BATCH_MAGS {
        let Some(mut from) = depot().dirty.pop() else {
            break;
        };
        // Not `debug_assert`: `[profile.release]` sets only `debug = true`, so a debug assert is
        // dead in exactly the arm the long soaks and every bench run in.
        assert!(spare.is_empty(), "framecache: zeroer destination not empty");
        while let Some(frame) = from.pop() {
            frame.zero();
            // `zero()` sets `PhysicalFrameFlags::ZEROED` as a side effect and this deliberately
            // undoes it. Cleanliness here is which magazine a frame is in; that flag means "in the
            // *physical* allocator's custody and known zero", and leaving it set on a cached frame
            // would survive hand-out, survive being dirtied, and then tell `raw_free_frame` to
            // file a dirty page on the zeroed free list -- a page of someone else's data served as
            // zeroed, which is the failure that killed the cache design before this one. Paid
            // here, on a background thread, rather than at hand-out where the old pool paid it.
            frame.set_not_zero();
            zeroed += 1;
            // Unreachable -- `spare` starts empty and `from` holds at most `MAG_SIZE` -- but the
            // return value is a frame, and discarding it would drop that frame on the floor
            // permanently. A lost frame is invisible: `CACHED_FRAMES` still counts it, the
            // physical allocator never sees it again, and it reads as a slow leak with no counter
            // to name it. Cheap to make loud at one check per frame on a background thread.
            assert!(
                spare.push(frame).is_none(),
                "framecache: zeroer destination overflowed"
            );
        }
        // `spare` is now the clean one and `from` is empty; swap roles and carry `from` forward as
        // the next iteration's destination.
        core::mem::swap(&mut spare, &mut from);
        // Routed by state, not by which branch produced it -- the same rule the refill path uses.
        // A dirty magazine can legitimately be empty (`install_side` and `refill` both push
        // partials), and filing an empty one on `clean` would hand `refill` a magazine with
        // nothing in it, which reads downstream as a cache miss rather than as a bookkeeping slip.
        if from.is_empty() {
            depot().empty.push(from);
        } else {
            depot().push_clean(from);
            mags += 1;
        }
    }
    depot().empty.push(spare);

    if zeroed > 0 {
        stat::BG_FRAMES.fetch_add(zeroed, Ordering::Relaxed);
        stat::BG_MAGS.fetch_add(mags, Ordering::Relaxed);
        stat::BG_CALLS.fetch_add(1, Ordering::Relaxed);
    } else {
        stat::BG_NO_DIRTY.fetch_add(1, Ordering::Relaxed);
    }
    zeroed > 0
}

// ---------------------------------------------------------------------------------------------
// Pressure
// ---------------------------------------------------------------------------------------------

/// Pressure level the trim should aim at, or `u8::MAX` for "no trim requested".
///
/// A flag rather than a direct call, and the reason is a recursion: the band transition is
/// detected in `recompute_memory_state`, which is reached from `free_frame_inner`, and trimming
/// frees frames -- straight back into `free_frame_inner`. Deferring to a thread breaks the cycle
/// without needing a re-entrancy guard, which is the kind of thing that is correct until someone
/// adds a second caller.
///
/// The cost is latency: the trim happens whenever `background_worker` next runs rather than at the
/// moment the band is crossed. Acceptable because the thing that must be immediate already is --
/// the free path stops *adding* to the cache at `Tight` on the spot, so what defers is only giving
/// back what is already held.
static TRIM_WANTED: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Note that memory pressure changed. Cheap enough for the band-transition path: one relaxed
/// store, no lock, no allocation, and safe to call from inside a free.
pub fn request_trim(state: super::tracker::MemoryState) {
    if !ENABLED {
        return;
    }
    TRIM_WANTED.store(state as usize, Ordering::Relaxed);
}

/// One unit of background work: a pending trim, else one magazine zeroed. Returns whether it did
/// anything, so the caller can block when there is nothing to do.
///
/// Called from `background_worker` -- the existing `Priority::BACKGROUND` kernel thread that
/// already does the equivalent job for the physical allocator. Trim first: giving memory back
/// under pressure outranks preparing more of it.
pub fn service() -> bool {
    if !ENABLED || !ready() {
        return false;
    }
    let wanted = TRIM_WANTED.swap(usize::MAX, Ordering::Relaxed);
    if wanted != usize::MAX {
        trim(match wanted {
            0 => super::tracker::MemoryState::Plenty,
            1 => super::tracker::MemoryState::Loaded,
            2 => super::tracker::MemoryState::Tight,
            _ => super::tracker::MemoryState::Emergency,
        });
        return true;
    }
    background_zero_one()
}

/// Return whole magazines to the physical allocator, down to what `state` permits.
///
/// Wired to the [`MemoryState`] band transition rather than to `ReclaimThread`, whose steps 1-5
/// are documented unimplemented and which therefore cannot be relied on to run at all. This is the
/// only thing that gives cached memory back, and the pool it replaces got that wrong in a way
/// worth restating: its ceiling and its trim target were the same constant, so its excess was
/// identically zero and it never drained once for the life of a boot.
///
/// Bounded per call. Frames are freed **outside** the depot lock: `free_frame_nopark` takes the
/// physical allocator's lock, and holding a leaf lock across it is the ordering this file forbids.
pub fn trim(state: super::tracker::MemoryState) {
    use super::tracker::MemoryState;
    if !ready() {
        return;
    }
    // Magazines to leave in the depot. `Plenty` still trims, unlike the pool's `Plenty` arm --
    // that is the F15/F16 defect, and a target equal to the bound is the same thing as no target.
    let target = match state {
        MemoryState::Plenty => MAX_MAGAZINES * 3 / 4,
        MemoryState::Loaded => MAX_MAGAZINES / 4,
        MemoryState::Tight => MAX_MAGAZINES / 16,
        MemoryState::Emergency => 0,
    };
    /// One magazine per call under pressure, so a band transition cannot turn into a
    /// multi-millisecond free loop on whichever thread happened to cross it.
    const MAGS_PER_TRIM: usize = 4;

    for _ in 0..MAGS_PER_TRIM {
        let mut d = depot();
        if d.clean.len() + d.dirty.len() <= target {
            return;
        }
        // Dirty first: a clean magazine is worth more, having already been paid for.
        let Some(mag) = d.dirty.pop().or_else(|| d.pop_clean()) else {
            return;
        };
        drop(d);

        let mut n = 0;
        while let Some(frame) = mag.pop() {
            if frame.clear_pooled() {
                CACHED_FRAMES.fetch_sub(1, Ordering::Relaxed);
            }
            super::tracker::free_frame_nopark(frame);
            n += 1;
        }
        stat::TRIMMED.fetch_add(n, Ordering::Relaxed);
        depot().empty.push(mag);
    }
}

/// Frames the cache can certainly absorb right now.
///
/// Read by the tracker before an over-fetch from the physical allocator: when a draw has to take
/// the allocator lock anyway, it takes a magazine's worth under that one acquisition and lets the
/// surplus land here, which is what makes the *next* `MAG_SIZE - 1` draws local hits.
///
/// **A deliberate underestimate.** Only whole empty magazines count; room left in some cpu's
/// partly-filled dirty magazine does not, because this cannot see other cpus and must not guess.
/// Reporting low costs a missed over-fetch. Reporting high costs the failure the old pool
/// measured: a surplus the cache refuses comes back out through `free_frame_nopark` **one frame at
/// a time**, which was `leftover=2,274,529`, fifteen global allocations per fault, and a bench 6.7x
/// slower. Bulk in with singular out is worse than no bulk at all, so this errs the safe way.
///
/// Takes the depot lock. That is affordable exactly because of where it is called: the caller is
/// about to take the physical allocator's lock, which costs orders more.
pub fn headroom() -> usize {
    if !ENABLED || !ready() {
        return 0;
    }
    // Net of [`ZERO_RESERVE`], for the same reason the free path is: an over-fetch sized against
    // magazines the free path may not touch comes back out through the physical allocator one
    // frame at a time, which is the failure this function's doc comment exists to avoid.
    DEPOT.lock().empty.len().saturating_sub(ZERO_RESERVE) * MAG_SIZE
}

/// Depth, for diagnostics. Racy by construction and labelled as such: three stacks read under one
/// lock are consistent with each other, but every cpu's private magazines are not in the count.
pub fn depths() -> (usize, usize, usize) {
    if !ready() {
        return (0, 0, 0);
    }
    let d = DEPOT.lock();
    (d.clean.len(), d.dirty.len(), d.empty.len())
}
