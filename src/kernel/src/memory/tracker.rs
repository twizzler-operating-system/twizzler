use alloc::vec::Vec;
use core::{
    alloc::Layout,
    sync::atomic::{AtomicUsize, Ordering},
};

use bitflags::bitflags;
use intrusive_collections::{intrusive_adapter, LinkedList};
use twizzler_abi::{pager::PhysRange, thread::ExecutionState};

use super::{
    frame::{get_frame, FrameRef, PhysicalFrameFlags, PHYS_LEVEL_LAYOUTS},
    PhysAddr,
};
use crate::{
    arch::memory::frame::FRAME_SIZE,
    condvar::CondVar,
    once::Once,
    processor::sched::{schedule, SchedFlags},
    spinlock::Spinlock,
    syscall::sync::finish_blocking,
    thread::{current_thread_ref, entry::start_new_kernel, priority::Priority, Thread, ThreadRef},
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
    reclaim: Once<ReclaimThread>,
    waiters: Spinlock<LinkedList<LinkAdapter>>,
}
intrusive_adapter!(pub LinkAdapter = ThreadRef: Thread { mutex_link: intrusive_collections::linked_list::AtomicLink });

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
        let Some(current_thread) = current_thread_ref() else {
            panic!("warning -- cannot wait on memory before threading initialized");
        };
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let guard = current_thread.enter_critical();
        self.waiters.lock().push_back(current_thread.clone());
        self.trigger_reclaim();
        {
            current_thread.set_state(ExecutionState::Sleeping);
            if self.idle() == old_idle {
                finish_blocking(guard);
            }
            current_thread.set_state(ExecutionState::Running);
        }
        self.waiting.fetch_sub(1, Ordering::SeqCst);
    }

    fn wake(&self) {
        let mut waiters = self.waiters.lock();
        while let Some(waiter) = waiters.pop_back() {
            crate::processor::sched::schedule_thread(waiter);
        }
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

/// Free a physical frame.
///
/// If the frame's flags indicates that it is zeroed, it will be placed on
/// the zeroed list.
pub fn free_frame(frame: FrameRef) {
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

pub fn reclaim(frames: impl IntoIterator<Item = FrameRef>) {
    TRACKER
        .poll()
        .unwrap()
        .reclaim
        .poll()
        .unwrap()
        .state
        .lock()
        .extend(frames);
    TRACKER.poll().unwrap().reclaim.poll().unwrap().cv.signal();
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
    let rt = tracker.reclaim.poll().unwrap();
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
        reclaim: Once::new(),
        waiters: Spinlock::new(LinkedList::new(LinkAdapter::NEW)),
    });
}

pub struct FrameAllocator {
    flags: FrameAllocFlags,
    layout: Layout,
    frames: Vec<FrameRef>,
}

impl FrameAllocator {
    pub fn new(flags: FrameAllocFlags, layout: Layout) -> Self {
        FrameAllocator {
            flags,
            layout,
            frames: Vec::new(),
        }
    }

    pub fn try_allocate(&mut self) -> Option<FrameRef> {
        if self.frames.len() == 0 {
            try_alloc_frame(self.flags, self.layout)
        } else {
            self.frames.pop()
        }
    }

    pub fn abort(&mut self, frames: impl IntoIterator<Item = FrameRef>) {
        for frame in frames {
            self.frames.push(frame);
        }
    }
}

impl Drop for FrameAllocator {
    fn drop(&mut self) {
        for frame in self.frames.drain(..) {
            free_frame(frame);
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
