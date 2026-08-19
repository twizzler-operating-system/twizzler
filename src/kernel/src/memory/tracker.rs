use alloc::vec::Vec;
use core::{
    alloc::Layout,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use bitflags::bitflags;
use intrusive_collections::{LinkedList, intrusive_adapter};
use twizzler_abi::{pager::PhysRange, thread::ExecutionState};

use super::{
    PhysAddr,
    frame::{FrameRef, PHYS_LEVEL_LAYOUTS, PhysicalFrameFlags, get_frame, split_frame},
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
    /// buys exposure, not speed; the win-shaped successor is a persistent per-cpu cache that
    /// amortizes the lock across operations rather than within one.
    pub const BULK_PRECHARGE: bool = true;

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

impl MemoryTracker {
    fn free_frame(&self, frame: FrameRef) {
        allocprofile::add(&allocprofile::FREES, 1);
        let count = frame.size() / FRAME_SIZE;
        let old = if frame.is_kernel() {
            self.kernel_used.fetch_sub(count, Ordering::SeqCst)
        } else {
            self.page_data.fetch_sub(count, Ordering::SeqCst)
        };
        assert!(old > 0);
        self.idle.fetch_add(count, Ordering::SeqCst);
        self.freed.fetch_add(count, Ordering::SeqCst);
        crate::memory::frame::raw_free_frame(frame);
        self.wake();
    }

    fn try_alloc_frame(&self, flags: FrameAllocFlags, layout: Layout) -> Option<FrameRef> {
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
        out: &mut Vec<FrameRef>,
    ) -> usize {
        if want == 0 {
            return 0;
        }
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
                return 0;
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
        let got = crate::memory::frame::raw_alloc_frames(pff, layout, reserved, out);
        allocprofile::add(&allocprofile::ALLOCS, got as u64);

        // Hand back what was reserved and not taken, or `idle` leaks by the difference.
        if got < reserved {
            self.idle.fetch_add((reserved - got) * per, Ordering::SeqCst);
        }
        if got == 0 {
            return 0;
        }
        for frame in &out[before..] {
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
        got
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
pub fn tracker_snapshot() -> (usize, usize, usize, bool) {
    let Some(t) = TRACKER.poll() else {
        return (0, 0, 0, false);
    };
    (t.idle(), t.page_data(), t.kernel_used(), t.should_reclaim())
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
    out: &mut Vec<FrameRef>,
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
    /// advisory; [MemoryTracker::trigger_reclaim] consults it on every allocation and must not take
    /// a lock to do so.
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
    // Provenance line for sweep logs: only a build with the flag on can emit it, so a log's
    // allocator configuration is decidable from the log alone.
    if allocprofile::BULK_PRECHARGE {
        logln!("allocprofile: BULK_PRECHARGE enabled");
    }
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
}

const MAX_FA_FRAMES: usize = 32;

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

pub struct FrameAllocator {
    flags: FrameAllocFlags,
    layout: Layout,
    abort: heapless::Vec<FrameRef, MAX_FA_FRAMES>,
    precharge: alloc::vec::Vec<FrameRef>,
    avoid_alloc: bool,
}

impl FrameAllocator {
    pub const fn new(flags: FrameAllocFlags, layout: Layout) -> Self {
        FrameAllocator {
            flags,
            layout,
            abort: heapless::Vec::new(),
            precharge: alloc::vec::Vec::new(),
            avoid_alloc: false,
        }
    }

    pub fn merge(&mut self, other: &mut Self) {
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
        if self.precharge.is_empty() {
            core::mem::swap(&mut self.precharge, &mut other.precharge);
        } else {
            self.precharge.append(&mut other.precharge);
        }
        // Bounded at `MAX_FA_FRAMES`, so this one stays a copy.
        self.precharge.extend(other.abort.drain(..));
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
        if self.precharge.len() >= count {
            allocprofile::add(&allocprofile::PRECHARGE_EARLY, 1);
            return;
        }
        self.precharge.reserve(count);
        let all_flags = self.flags | flags;
        let mut remaining = count - self.precharge.len();
        if allocprofile::BULK_PRECHARGE {
            // One acquisition for the batch.
            let got = try_alloc_frames(all_flags, self.layout, remaining, &mut self.precharge);
            allocprofile::add(&allocprofile::PRECHARGE_FETCHED, got as u64);
            remaining -= got;
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
        if !self.abort.is_empty() {
            return self.abort.pop();
        }
        if self.precharge.len() == 0 {
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
        }
    }

    pub fn abort(&mut self, frames: impl IntoIterator<Item = FrameRef>) {
        for frame in frames {
            if self.abort.push(frame).is_err() {
                log::warn!(
                    "frame allocator abort: too many frames to store, dropping frame {:?}",
                    frame
                );
            }
        }
    }

    pub fn clear(&mut self) {
        while let Some(frame) = self.abort.pop() {
            free_frame(frame);
        }
        while let Some(frame) = self.precharge.pop() {
            free_frame(frame);
        }
    }

    /// Return up to [`TRIM_PER_DROP`] frames held above [`MAX_TLS_PRECHARGE`] to the allocator.
    ///
    /// Runs before the pool goes back to thread-local storage, so the frames it gives up are ones
    /// no operation asked for.
    fn trim(&mut self) {
        let mut excess = self
            .precharge
            .len()
            .saturating_sub(MAX_TLS_PRECHARGE)
            .min(TRIM_PER_DROP);
        while excess > 0 {
            let Some(frame) = self.precharge.pop() else {
                break;
            };
            allocprofile::add(&allocprofile::FA_TRIMMED, 1);
            free_frame(frame);
            excess -= 1;
        }
    }
}

#[thread_local]
static mut TLS_FRAME_ALLOCATOR: Option<FrameAllocator> = None;
static TLS_FRAME_ALLOCATOR_LOCK: AtomicBool = AtomicBool::new(false);

fn try_lock_tls_frame_allocator() -> bool {
    !TLS_FRAME_ALLOCATOR_LOCK.swap(true, Ordering::SeqCst)
}

fn unlock_tls_frame_allocator() {
    TLS_FRAME_ALLOCATOR_LOCK.store(false, Ordering::SeqCst);
}

#[allow(static_mut_refs)]
pub fn save_frame_allocator(fa: &mut FrameAllocator) -> bool {
    crate::interrupt::with_disabled(|| {
        if try_lock_tls_frame_allocator() {
            unsafe {
                if let Some(ref mut tls_fa) = TLS_FRAME_ALLOCATOR {
                    tls_fa.merge(fa);
                } else {
                    TLS_FRAME_ALLOCATOR = Some(FrameAllocator::new(
                        FrameAllocFlags::ZEROED | FrameAllocFlags::KERNEL,
                        PHYS_LEVEL_LAYOUTS[0],
                    ));
                    TLS_FRAME_ALLOCATOR.as_mut().unwrap().merge(fa);
                    TLS_FRAME_ALLOCATOR.as_mut().unwrap().avoid_alloc = true;
                }
            }
            unlock_tls_frame_allocator();
            true
        } else {
            allocprofile::add(&allocprofile::FA_SAVE_LOCKED, 1);
            false
        }
    })
}

#[allow(static_mut_refs)]
pub fn count_precharged_frames() -> usize {
    if !tls_ready() {
        return 0;
    }
    crate::interrupt::with_disabled(|| {
        if try_lock_tls_frame_allocator() {
            let count = unsafe {
                TLS_FRAME_ALLOCATOR
                    .as_ref()
                    .map(|fa| fa.precharge.len())
                    .unwrap_or(0)
            };
            unlock_tls_frame_allocator();
            count
        } else {
            0
        }
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
    crate::interrupt::with_disabled(|| {
        if try_lock_tls_frame_allocator() {
            unsafe {
                let fa = TLS_FRAME_ALLOCATOR.take();
                unlock_tls_frame_allocator();
                if fa.is_none() {
                    allocprofile::add(&allocprofile::FA_TAKE_NONE, 1);
                }
                fa
            }
        } else {
            // The pool is `#[thread_local]`, i.e. per-cpu, but the flag guarding it is one global
            // atomic -- so this arm is another cpu holding it, and the caller goes on to build a
            // fresh empty allocator whose every precharge reaches the global tracker while this
            // cpu's own pool sits untouched. Counted to size that effect.
            allocprofile::add(&allocprofile::FA_TAKE_LOCKED, 1);
            None
        }
    })
}

pub fn take_or_new_frame_allocator() -> FrameAllocator {
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
        if tls_ready() && self.layout == PHYS_LEVEL_LAYOUTS[0] {
            self.trim();
            let t = allocprofile::start();
            let saved = save_frame_allocator(self);
            allocprofile::record(&allocprofile::FA_DROP_SAVE_NS, t);
            if !saved {
                allocprofile::add(&allocprofile::FA_DROP_CLEARED, 1);
                let t = allocprofile::start();
                self.clear();
                allocprofile::record(&allocprofile::FA_DROP_CLEAR_NS, t);
            } else {
                allocprofile::add(&allocprofile::FA_DROP_SAVED, 1);
            }
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
