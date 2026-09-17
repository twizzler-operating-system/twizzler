use alloc::vec::Vec;
use core::{
    alloc::Layout,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
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

pub mod allocprofile {
    use core::sync::atomic::{AtomicU64, Ordering};

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

    /// Precharge call-site tags, so over-fetch can be attributed instead of inferred.
    pub const PC_SITE_OTHER: u8 = 0;
    pub const PC_SITE_FILL: u8 = 1;
    pub const PC_SITE_MAP: u8 = 2;

    /// `(calls, want, unused)` counters for a site tag.
    pub fn pc_counters(site: u8) -> (&'static AtomicU64, &'static AtomicU64, &'static AtomicU64) {
        match site {
            PC_SITE_FILL => (&PC_FILL_CALLS, &PC_FILL_WANT, &PC_FILL_UNUSED),
            PC_SITE_MAP => (&PC_MAP_CALLS, &PC_MAP_WANT, &PC_MAP_UNUSED),
            _ => (&PC_OTHER_CALLS, &PC_OTHER_WANT, &PC_OTHER_UNUSED),
        }
    }

    counters!(
        ALLOCS,
        ZEROED_INLINE,
        WAITS,
        FREES,
        RECLAIM_SIGNALS,
        RECLAIM_WAKES,
        RECLAIM_ROUNDS,
        FILL_ITERS,
        FA_DROP_SAVED,
        FA_DROP_CLEARED,
        FA_DROP_FRAMES,
        FA_TRIMMED,
        FA_ALLOC_POOL,
        FA_ALLOC_GLOBAL,
        FA_ALLOC_AVOID_EMPTY,
        PRECHARGE_CALLS,
        PRECHARGE_EARLY,
        PRECHARGE_FETCHED,
        FA_POOL_ZEROED,
        FA_PARK_PRESSURE,
        PT_CHECKED,
        PT_DIRTY,
        ALLOC_BULK_FRAMES,
        ALLOC_BULK_CALLS,
        ALLOC_SINGLE_CALLS,
        FA_SPILL,
        PC_FILL_CALLS,
        PC_FILL_WANT,
        PC_FILL_UNUSED,
        PC_MAP_CALLS,
        PC_MAP_WANT,
        PC_MAP_UNUSED,
        PC_OTHER_CALLS,
        PC_OTHER_WANT,
        PC_OTHER_UNUSED,
    );

    pub fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
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
/// the page-table term. Bounded by the frame cache's depth per cpu.
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
        self.free_frame_inner(frame, true, false)
    }

    /// `allow_park = false` is for callers that are *draining* the cache: a caching free would
    /// push the frame straight back into the cache the caller is emptying.
    fn free_frame_inner(&self, frame: FrameRef, allow_park: bool, known_zero: bool) {
        allocprofile::add(&allocprofile::FREES, 1);
        let count = frame.size() / FRAME_SIZE;
        // Park before any accounting: a parked frame stays ALLOCATED and stays charged to its
        // class, which is exactly how every other frame in this pool is already treated, so the
        // free's counter writes are *skipped* rather than reproduced. No `wake()` either --
        // nothing became available to a waiter, which is consistent with parking stopping once
        // `MemoryState` reaches `Tight`, the only state in which waiters exist.
        if count == 1 && allow_park && cache_freed_frame_hinted(frame, known_zero) {
            return;
        }
        let old = if frame.is_kernel() {
            self.kernel_used.fetch_sub(count, Ordering::SeqCst)
        } else {
            self.page_data.fetch_sub(count, Ordering::SeqCst)
        };
        assert!(old > 0);
        self.idle.fetch_add(count, Ordering::SeqCst);
        self.freed.fetch_add(count, Ordering::SeqCst);
        // The cache declined this frame (pressure, or no magazine), so it goes to the physical
        // allocator -- which files it by `PhysicalFrameFlags::ZEROED` alone. That bit was cleared
        // when the frame was handed out, so without this a frame we *know* is zero lands on the
        // dirty free list and the background zeroer pays to zero it again. Setting it here is
        // sound for the same reason the caller's claim is: it is set at the moment of the free,
        // from a caller that has proved the contents, not carried over from an earlier life.
        if known_zero {
            frame.set_flags(crate::memory::frame::PhysicalFrameFlags::ZEROED, true);
        }
        crate::memory::frame::raw_free_frame(frame);
        self.note_idle_change();
        self.wake();
    }

    fn try_alloc_frame(&self, flags: FrameAllocFlags, layout: Layout) -> Option<FrameRef> {
        // This cpu's pool first. A parked frame is already `ALLOCATED`, already charged to a
        // class and never returned to `idle`, so serving one here skips `consider_reclaim`, the
        // `idle` CAS, the class and `allocated` counters, `note_idle_change` and the PFA lock --
        // which is 660 of the 703 ns a singular frame costs (`alloct1`; the remaining ~43 ns is
        // the 11.7%-weighted inline zeroing). `ALLOCS` deliberately does not count it:
        // they mean "went to the global allocator", and every ratio built on them reads them
        // that way.
        if layout == PHYS_LEVEL_LAYOUTS[0] {
            if let Some((frame, needs_zeroing)) = framecache::alloc_one(want_of(flags)) {
                return Some(finish_cached_alloc(frame, flags, needs_zeroing));
            }
        }
        let r = self.do_try_alloc_frame(flags, layout);
        allocprofile::add(&allocprofile::ALLOCS, 1);
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
        // which is a deterministic self-deadlock.
        let mut from_pool = 0;
        if layout == PHYS_LEVEL_LAYOUTS[0] {
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
        let want = if layout == PHYS_LEVEL_LAYOUTS[0] && memory_state() == MemoryState::Plenty {
            // Bounded by what the cache will actually absorb, so the surplus lands there instead
            // of bouncing off it.
            want.max(POOL_REFILL_BATCH.min(framecache::headroom()))
        } else {
            want
        };
        let pff = if flags.contains(FrameAllocFlags::ZEROED) {
            PhysicalFrameFlags::ZEROED
        } else {
            PhysicalFrameFlags::empty()
        };
        let per = layout.size() / FRAME_SIZE;
        self.consider_reclaim();

        // Reserve the whole batch against `idle` in one CAS. Reserving what is there rather than
        // failing outright keeps this a best-effort call: the caller asked for `want` and takes
        // what it gets.
        let reserved = loop {
            let idle = self.idle();
            let can = (idle / per).min(want);
            if can == 0 {
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

        out.reserve(reserved);
        let before = out.len();
        allocprofile::add(&allocprofile::ALLOC_BULK_CALLS, 1);
        let got = crate::memory::frame::raw_alloc_frames(pff, layout, reserved, out);
        allocprofile::add(&allocprofile::ALLOC_BULK_FRAMES, got as u64);
        allocprofile::add(&allocprofile::ALLOCS, got as u64);

        // Hand back what was reserved and not taken, or `idle` leaks by the difference.
        if got < reserved {
            self.idle
                .fetch_add((reserved - got) * per, Ordering::SeqCst);
        }
        if got == 0 {
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
                self.wait(idle);
                allocprofile::add(&allocprofile::WAITS, 1);
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
        {
            // Requested, never run here: the census takes object page-table mutexes, and this
            // thread may already hold one -- an allocation under a pt lock that exhausts memory
            // lands exactly here, and taking the census inline re-entered that lock (0/8 in
            // `many-reclaim0`, panic at the census's `lock_page_tables`). The reclaim thread
            // holds no object locks; it prints on the next wake. Every 4th wait, since a stack
            // of starving threads re-requesting buys nothing.
            static CENSUS_TICK: AtomicUsize = AtomicUsize::new(0);
            if CENSUS_TICK.fetch_add(1, Ordering::Relaxed) % 4 == 0 {
                CENSUS_REQUESTED.store(true, Ordering::Release);
            }
        }
        let Some(current_thread) = current_thread_ref() else {
            panic!("warning -- cannot wait on memory before threading initialized");
        };
        // Before registering, and before the critical section: the reaper may be holding whole
        // page-table chains it has not been woken for (see `obj::poke_reaper`), and a signal is
        // not something to issue with `enter_critical` held -- a last `ThreadRef` drop in there
        // panics.
        crate::obj::poke_reaper();
        // The *thread* reaper too, not just the object reaper. Its wake sources are the idle
        // loop and stattick safe-points, and a saturated machine has neither -- so it sleeps on
        // its condvar while exited threads pile up, each pinning a 2 MiB stack and its object
        // references. The reclaim1 census measured the end state: 92% of a full machine was
        // pages of pending-delete objects held live by unreaped threads. A thread about to
        // block for memory is exactly who should pay the wake.
        crate::thread::reaper::notify();
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
            }
            // Unlink on **both** paths, not just the didn't-block one.
            //
            // The didn't-block path is the obvious case: memory became available before we
            // committed, so `wake()` never drained us and a stale strong reference would be left
            // in `waiters` for some later, unrelated wake to reschedule.
            //
            // The blocking path needs it too. `finish_blocking` returns when *anything* makes this
            // thread runnable, and being woken through the requeue list rather than by `wake()`
            // draining `waiters` -- a force-exit, say -- leaves the link set. Nothing unlinked it,
            // so the next time this thread waits for memory it pushes an already-linked node and
            // `intrusive_collections` panics from `node_from_value`. That is the "already linked"
            // panic seen in `try_alloc_frame`, which is this list and not the frame pool's.
            //
            // Safe as a no-op in the common case *because* `wake()` pops rather than detaching:
            // `pop_front` clears the link, so `is_linked()` here answers about this list and not
            // about a list that was moved out from under it. See the note there.
            {
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

    /// How many waiters one lock hold claims. Bounded for the same reason `requeue_all`'s batch
    /// is: the pops happen under a spinlock and the requeue must not.
    const WAKE_BATCH: usize = 8;

    fn wake(&self) {
        let g = current_thread_ref().map(|ct| ct.enter_critical());
        // Popped under the lock, requeued outside it -- see `Request::signal`, which is the same
        // shape for the same reason.
        //
        // **Popped, not `take()`n.** `LinkedList::take` moves the list's head and tail and leaves
        // every node's link alone, so a thread on the detached list still reads
        // `memwait_link.is_linked() == true`. The `else` arm in [`Self::wait`] tests exactly that
        // before unlinking itself, under this same lock -- and its comment argues that holding the
        // lock makes the two safe against each other. It does not, once the list has been
        // detached: `wait` finds itself linked, builds a cursor into `self.waiters` (now empty)
        // from its own node, and splices the node out by rewriting neighbours that belong to the
        // detached list. That corrupts the list this function is about to walk and releases a
        // reference the detached list still owns, which is a walk off a freed `Thread`.
        //
        // `pop_front` clears the node's link, so `wait`'s `is_linked()` test means what its
        // comment says it means and the no-op case is a real no-op.
        //
        // Tested for emptiness before anything else is built. This runs on **every frame free**
        // (`free_frame_inner`) and a memory waiter is rare -- one contended run recorded 3.45M lock
        // acquisitions with nobody ever waiting -- so the no-waiter path has
        // to cost one lock and a bool. Building the batch unconditionally cost ~70 ns per free,
        // which is 4 frees per `object_create_delete_nomap` iteration and measured as +4.5% on it.
        if !self.waiters.lock().is_empty() {
            loop {
                let mut batch = heapless::Vec::<ThreadRef, { Self::WAKE_BATCH }>::new();
                {
                    let mut waiters = self.waiters.lock();
                    while !batch.is_full() {
                        let Some(thread) = waiters.pop_front() else {
                            break;
                        };
                        // Safety: not full, checked above.
                        unsafe { batch.push_unchecked(thread) };
                    }
                }
                if batch.is_empty() {
                    break;
                }
                let full = batch.is_full();
                add_all_to_requeue(batch);
                if !full {
                    break;
                }
            }
        }
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
            if reclaim.queued.load(Ordering::Relaxed) == 0
                // A pending pressure census is work only this thread can do (it takes object
                // pt locks the requester may hold), so it pierces the no-work gate. Rare by
                // construction: set only from the allocator's wait path.
                && !CENSUS_REQUESTED.load(Ordering::Acquire)
            {
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

/// Total frames the allocator manages, for callers sizing a budget as a share of the machine
/// rather than a fixed count.
pub fn total_frames() -> usize {
    TRACKER.poll().map(|t| t.total()).unwrap_or(0)
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
    let pooled = pooled_frames();
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
    // `pooled` is charged to `kernel`/`page` above like any other allocated frame, and
    // `pooled_frames` exists precisely so a reader can subtract it -- but this print, which is the
    // only instrument available once the allocator is parking its callers, never showed it. So the
    // largest bar in a wedge transcript (`kernel` at 40% of 845 MB / 71% of 333 MB in the lowmem
    // ladder) could not be split into pool occupancy versus live use from the log alone.
    logln!("      pooled: {} {}%", pooled, (pooled * 100) / total);
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

/// A frame that is already zero, or `None` -- never a dirty frame plus a memset.
///
/// Served only from the frame cache's clean side. The physical allocator is deliberately not
/// consulted on a miss: reaching it means its lock and its dirty fallback, which is the work this
/// exists to avoid. A caller that gets `None` is expected to have a cheaper alternative, not to
/// try harder.
pub fn try_alloc_prezeroed_frame(layout: Layout) -> Option<FrameRef> {
    if layout != PHYS_LEVEL_LAYOUTS[0] {
        return None;
    }
    let frame = framecache::alloc_one_clean()?;
    // `needs_zeroing: false` is the point: the cache only reports a frame clean when it is.
    Some(finish_cached_alloc(frame, FrameAllocFlags::ZEROED, false))
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

/// [`free_frame`], where the caller can prove nothing has written the frame since it was zeroed.
///
/// The only caller is the unmap path, for a `PROBED` entry whose hardware dirty bit is clear:
/// installed zeroed, with the map-time `DIRTY` suppressed so the hardware bit is meaningful.
pub fn free_frame_known_zero(frame: FrameRef) {
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
        .free_frame_inner(frame, true, true)
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
        .free_frame_inner(frame, false, false)
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
/// the frame cache with a resumed background worker.
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

/// Set by a starving thread in [MemoryTracker::wait]; consumed by `reclaim_main`, which runs
/// [crate::obj::pressure_census] lock-safely. Never run the census from the wait path.
static CENSUS_REQUESTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

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
            th: start_new_kernel(Priority::BACKGROUND, reclaim_start, 0, "mem-reclaim"),
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
                // Step 1: clean, pager-backed object memory. Object granularity and clean-only --
                // a clean backed page can be dropped outright because the pager can produce it
                // again, while a dirty one needs a write-back and anonymous memory needs swap,
                // which does not exist. See `reclaim-design.md`.
                //
                // Intensity from the band, not from `should_reclaim()`: the latter is
                // `page_cond() || kern_cond()` and `page_cond` latches true early in a boot and
                // never recovers, so it is usable as "may I run" and useless as "how hard".
                // A *page* budget now, not an object count: page-level eviction means the unit
                // of work is a page, and the old 1/4/16 would have been 1/4/16 pages a round.
                let budget = match memory_state() {
                    MemoryState::Plenty => 0,
                    MemoryState::Loaded => 64,
                    MemoryState::Tight => 256,
                    MemoryState::Emergency => 1024,
                };
                if budget > 0 {
                    // Outside the state lock: eviction takes object page-table locks and sends
                    // shootdown IPIs, neither of which may happen under a spinlock.
                    drop(state);
                    let freed = crate::obj::reclaim_clean_backed(budget);
                    state = rt.state.lock();
                    // Printed whenever a scan ran, not only when it freed something. The
                    // page-granularity ladder printed *nothing* at 896 and 768, which said zero
                    // pages moved and nothing about why -- the skip breakdown is the whole value
                    // of these counters and it was gated behind the success case.
                    crate::obj::reclaimstat::print();
                    if freed > 0 {
                        count += freed;
                        thisround += freed;
                    }
                }
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
            // thread preempting it.
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
        // Requested by starving threads in `MemoryTracker::wait`, run here because this thread
        // holds no object page-table locks (the requester may -- taking the census inline in
        // `wait` re-entered a held pt mutex, 0/8 in `many-reclaim0`). After the free pass, so
        // queued frames drain before the census can block on a pt mutex whose holder is itself
        // waiting for memory; outside the state spinlock, since the census takes sleeping
        // mutexes.
        if CENSUS_REQUESTED.swap(false, Ordering::AcqRel) {
            drop(state);
            crate::obj::pressure_census();
            state = rt.state.lock();
        }
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

/// Frames taken per global-allocator acquisition when the pool has to be refilled. Sets the
/// fraction of allocations that touch the PFA lock: ~1 in `POOL_REFILL_BATCH`.
const POOL_REFILL_BATCH: usize = 64;

/// Inline capacity for a precharge list, spilling to the heap only when something asks for more.
///
/// Every operation gets a *fresh* `FrameAllocator`, so `precharge`'s eager reserve
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
/// cost of moving the old pool by value -- `take_fa` fell 114 -> 9 ns by not moving a ~300-byte
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
/// `raw_alloc_frames` -- already checks before pushing.
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
    /// Call-site tag; see `allocprofile::PC_SITE_*`.
    site: u8,
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
            site: allocprofile::PC_SITE_OTHER,
        }
    }

    /// Tag this allocator's precharges for attribution. See `allocprofile::PC_SITE_*`.
    pub fn set_site(&mut self, site: u8) {
        self.site = site;
    }

    #[track_caller]
    pub fn precharge(&mut self, count: usize, flags: FrameAllocFlags) {
        {
            let (c, w, _) = allocprofile::pc_counters(self.site);
            allocprofile::add(c, 1);
            allocprofile::add(w, count as u64);
        }
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
        // Exactly `count`: `FA_INLINE_CAP` is 8 and every hot-path caller asks for 1-4, so this is
        // a capacity check against inline storage and reaches the kernel heap not at all.
        // `setup_cow_range`'s ~1,030 still spills, once, and is rare enough to leave.
        self.precharge
            .reserve(count.saturating_sub(self.precharge.len()));
        if self.precharge.len() >= count {
            allocprofile::add(&allocprofile::PRECHARGE_EARLY, 1);
            return;
        }
        let all_flags = self.flags | flags;
        let mut remaining = count - self.precharge.len();
        // One acquisition for the batch.
        let got = try_alloc_frames(all_flags, self.layout, remaining, &mut self.precharge);
        allocprofile::add(&allocprofile::PRECHARGE_FETCHED, got as u64);
        // `saturating_sub`, not `-`: the batch may deliberately return **more** than was asked
        // for. `[profile.release]` leaves
        // overflow checks off, so a plain subtraction would wrap to `usize::MAX` here and the
        // loop below would try to allocate the machine.
        remaining = remaining.saturating_sub(got);
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
        {
            let (c, w, _) = allocprofile::pc_counters(self.site);
            allocprofile::add(c, 1);
            allocprofile::add(w, count as u64);
        }
        if self.precharge.len() >= count {
            allocprofile::add(&allocprofile::PRECHARGE_EARLY, 1);
        }
        let want = count.saturating_sub(self.precharge.len());
        if want > 0 {
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
    if !tls_ready() {
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
    // Mirrors `raw_free_frame`, which clears both bits on the way to the physical allocator. The
    // frame cache is a second, parallel free path that bypasses it, so a page-table frame freed
    // into a magazine kept `IS_PT` set, was handed straight back out to a consumer, and tripped
    // `assert!(!frame.is_pt())` where the pager installs page data (`pager/queues.rs`). Both bits
    // describe the *previous* owner's use and are meaningless once the frame is free.
    frame.set_pt(false);
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
    /// `?` and never aborts. With the frame cache, `Drop` reaches this on the level-0 path too
    /// (the old pool's `merge` used to absorb the abort list first), so a level-0 abort after an
    /// rc bump would trip `free_frame`'s assert here.
    ///
    /// Pre-existing; documented rather than fixed so it is not re-derived from two files.
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
}

/// An allocator for one operation. Fresh and empty: it draws from the frame cache as it
/// precharges, and `Drop` hands its surplus back.
pub fn take_or_new_frame_allocator() -> FrameAllocator {
    let mut fa = FrameAllocator::new(
        FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
        PHYS_LEVEL_LAYOUTS[0],
    );
    fa.avoid_alloc = true;
    fa
}

impl Drop for FrameAllocator {
    fn drop(&mut self) {
        allocprofile::add(
            &allocprofile::FA_DROP_FRAMES,
            (self.precharge.len() + self.abort.len()) as u64,
        );
        {
            // Leftover precharge is exactly the over-fetch: fetched, never popped, handed back.
            let (_, _, u) = allocprofile::pc_counters(self.site);
            allocprofile::add(u, self.precharge.len() as u64);
        }
        // Note that the abort list is recycled by the save/take path below rather than freed:
        // abort frames can carry a non-zero refcount (a failed map after an rc bump), which
        // `free_frame` refuses outright. Parking feeds only from `free_frame` (rc==0 asserted).
        if tls_ready() && self.layout == PHYS_LEVEL_LAYOUTS[0] {
            // Surplus goes back to the cache: an operation borrows and returns, and between
            // operations the frames live somewhere every other draw and every other free on this
            // cpu can reach. Only the precharge list: abort frames are already marked `POOLED` by
            // `abort()`, so offering one here would trip the cache's double-mark assert, and some
            // carry a non-zero refcount; they take the `clear()` path below.
            //
            // Clean, not dirty. These were never popped by `try_allocate`, so nobody wrote them;
            // returning them as dirty makes the cache memset a frame it is about to hand straight
            // back, which is 99.98% of hand-outs on `object_map_unmap_syscall`.
            let known_zero = self.precharge_known_zero;
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
        } else {
            allocprofile::add(&allocprofile::FA_DROP_CLEARED, 1);
        }
        self.clear();
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
