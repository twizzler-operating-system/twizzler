use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use bitflags::bitflags;
use intrusive_collections::{intrusive_adapter, LinkedList};
use twizzler_abi::thread::ExecutionState;

use super::{
    frame::{FrameRef, PhysicalFrameFlags},
    MemoryRegion,
};
use crate::{
    arch::memory::frame::FRAME_SIZE,
    condvar::CondVar,
    once::Once,
    spinlock::Spinlock,
    syscall::sync::finish_blocking,
    thread::{current_thread_ref, entry::start_new_kernel, priority::Priority, Thread, ThreadRef},
};

pub struct MemoryTracker {
    kernel_used: AtomicUsize,
    page_data: AtomicUsize,
    idle: AtomicUsize,
    total: AtomicUsize,
    pager_outstanding: AtomicUsize,
    reclaim: Once<ReclaimThread>,
    waiters: Spinlock<LinkedList<LinkAdapter>>,
}
intrusive_adapter!(pub LinkAdapter = ThreadRef: Thread { mutex_link: intrusive_collections::linked_list::AtomicLink });

impl MemoryTracker {
    fn free_frame(&self, frame: FrameRef) {
        let old = if frame.is_kernel() {
            self.kernel_used.fetch_sub(1, Ordering::SeqCst)
        } else {
            self.page_data.fetch_sub(1, Ordering::SeqCst)
        };
        assert!(old > 0);
        self.idle.fetch_add(1, Ordering::SeqCst);
        crate::memory::frame::raw_free_frame(frame);
        self.wake();
    }

    fn try_alloc_frame(&self, flags: FrameAllocFlags) -> Option<FrameRef> {
        let pff = if flags.contains(FrameAllocFlags::ZEROED) {
            PhysicalFrameFlags::ZEROED
        } else {
            PhysicalFrameFlags::empty()
        };
        loop {
            if let Some(frame) = crate::memory::frame::raw_try_alloc_frame(pff) {
                if flags.contains(FrameAllocFlags::KERNEL) {
                    frame.set_kernel(true);
                    self.kernel_used.fetch_add(1, Ordering::SeqCst);
                } else {
                    frame.set_kernel(false);
                    self.page_data.fetch_add(1, Ordering::SeqCst);
                }
                let old = self.idle.fetch_sub(1, Ordering::SeqCst);
                assert!(old > 0);
                return Some(frame);
            }

            if flags.contains(FrameAllocFlags::WAIT_OK) {
                self.wait();
            } else {
                return None;
            }
        }
    }

    fn alloc_frame(&self, flags: FrameAllocFlags) -> FrameRef {
        self.try_alloc_frame(flags).expect("cannot wait for page")
    }

    fn wait(&self) {
        self.trigger_reclaim();
        let Some(current_thread) = current_thread_ref() else {
            logln!("warning -- cannot wait on memory before threading initialized");
            return;
        };
        let guard = current_thread.enter_critical();
        current_thread.set_state(ExecutionState::Sleeping);
        self.waiters.lock().push_back(current_thread.clone());
        finish_blocking(guard);
        current_thread.set_state(ExecutionState::Running);
    }

    fn wake(&self) {
        let mut waiters = self.waiters.lock();
        while let Some(waiter) = waiters.pop_back() {
            crate::sched::schedule_thread(waiter);
        }
    }

    fn trigger_reclaim(&self) {
        if let Some(reclaim) = self.reclaim.poll() {
            reclaim.cv.signal();
        } else {
            logln!("warning -- cannot trigger reclaim thread before it is started");
        }
    }

    fn consider_reclaim(&self) {
        if self.should_reclaim() {
            self.trigger_reclaim();
        }
    }

    fn should_reclaim(&self) -> bool {
        let idle = self.idle();
        let kern = self.kernel_used();
        let page = self.page_data();

        let k2 = kern * 2;
        let split_idle = idle / 2;

        logln!(
            "should reclaim? {} {} {}, {}",
            idle,
            kern,
            page,
            page >= split_idle || idle < k2
        );

        page >= split_idle || idle < k2
    }

    fn idle(&self) -> usize {
        self.idle.load(Ordering::Acquire)
    }

    fn kernel_used(&self) -> usize {
        self.kernel_used.load(Ordering::Acquire)
    }

    fn page_data(&self) -> usize {
        self.page_data.load(Ordering::Acquire)
    }

    fn track_frame_pager(&self) {
        self.pager_outstanding.fetch_add(1, Ordering::SeqCst);
    }

    fn untrack_frame_pager(&self) {
        self.pager_outstanding.fetch_sub(1, Ordering::SeqCst);
    }

    fn pager_outstanding(&self) -> usize {
        self.pager_outstanding.load(Ordering::SeqCst)
    }
}

pub static TRACKER: Once<MemoryTracker> = Once::new();

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
pub fn try_alloc_frame(flags: FrameAllocFlags) -> Option<FrameRef> {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .try_alloc_frame(flags)
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
pub fn track_page_pager(_frame: FrameRef) {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .track_frame_pager()
}

/// Track a page as owned by the pager.
pub fn untrack_page_pager(_frame: FrameRef) {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .untrack_frame_pager()
}

/// Get outstanding pager pages
pub fn get_outstanding_pager_pages() -> usize {
    TRACKER
        .poll()
        .expect("page tracker not initialized")
        .pager_outstanding()
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
    state: Spinlock<()>,
    cv: CondVar,
}

impl ReclaimThread {
    fn new() -> Self {
        extern "C" fn reclaim_start() {
            reclaim_main();
        }
        Self {
            th: start_new_kernel(Priority::REALTIME, reclaim_start, 0),
            state: Spinlock::new(()),
            cv: CondVar::new(),
        }
    }
}

fn reclaim_main() {
    let tracker = TRACKER.wait();
    let rt = tracker.reclaim.wait();
    let mut state = rt.state.lock();
    loop {
        logln!("reclaim thread triggered");
        state = rt.cv.wait(state);
    }
}

pub fn init(regions: &[MemoryRegion]) {
    TRACKER.call_once(|| {
        let total = regions.iter().fold(0, |acc, x| acc + x.length / FRAME_SIZE);
        MemoryTracker {
            kernel_used: AtomicUsize::new(0),
            page_data: AtomicUsize::new(0),
            idle: AtomicUsize::new(total),
            total: AtomicUsize::new(total),
            pager_outstanding: AtomicUsize::new(0),
            reclaim: Once::new(),
            waiters: Spinlock::new(LinkedList::new(LinkAdapter::NEW)),
        }
    });
}

pub struct FrameAllocator {
    flags: FrameAllocFlags,
    frames: Vec<FrameRef>,
}

impl FrameAllocator {
    pub fn new(flags: FrameAllocFlags) -> Self {
        FrameAllocator {
            flags,
            frames: Vec::new(),
        }
    }

    pub fn try_allocate(&mut self) -> Option<FrameRef> {
        if self.frames.len() == 0 {
            try_alloc_frame(self.flags)
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
