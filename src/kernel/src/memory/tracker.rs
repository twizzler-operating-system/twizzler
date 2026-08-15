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
                self.wait(idle);
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
    rt.state.lock().extend(frames);
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

struct ReclaimThread {
    th: ThreadRef,
    state: Spinlock<Vec<FrameRef>>,
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
        while tracker.should_reclaim() {
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

            if thisround < MAX_PER_ROUND {
                // TODO
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
        // Extend our precharge with abort, since we can just use them.
        self.precharge.extend(other.abort.drain(..));
        self.precharge.append(&mut other.precharge);
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
        if self.precharge.len() >= count {
            return;
        }
        self.precharge.reserve(count);
        let remaining = count - self.precharge.len();
        for _ in 0..remaining {
            let Some(frame) = try_alloc_frame(self.flags | flags, self.layout) else {
                return;
            };
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
        while self.precharge.len() < count {
            let Some(frame) = try_alloc_frame(self.flags & !FrameAllocFlags::WAIT_OK, self.layout)
            else {
                break;
            };
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
            if self.avoid_alloc {
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
                fa
            }
        } else {
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
        if tls_ready() && self.layout == PHYS_LEVEL_LAYOUTS[0] {
            if !save_frame_allocator(self) {
                self.clear();
            }
        } else {
            self.clear();
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
