//! Reaping exited threads.
//!
//! A thread cannot drop its own last reference -- it is still running on the kernel stack that drop
//! hands back to the free list -- so the drop is deferred to another context. Both pre-existing
//! contexts are constrained, and for good reasons:
//!
//! - `schedule_stattick` reaps at most one thread per tick, and only when the interrupted thread
//!   `is_in_user()`, is not critical and holds no mutex. That is a safe-point test, not a throttle:
//!   `Thread::drop` -> `IdCounter::release` takes a *sleeping* mutex.
//! - the idle loop reaps one per hundred passes. Blocking on that mutex from an idle thread is the
//!   wedge `schedule` documents at length -- an idle thread descheduled holding a mutex is a lock
//!   owner nothing can schedule.
//!
//! Both restrictions are anti-correlated with thread churn. A spawn/join workload is in the kernel
//! or blocked for nearly all of its time, so the ticks that would reap keep landing somewhere that
//! skips, and an idle cpu never satisfies `is_in_user()` at all. Measured: ~11% of spawns left
//! unreaped, each pinning its `Thread` allocation and a 2 MiB kernel stack, ~130 MiB per 660
//! spawn+joins and never returned (leakcheck.md, leak29).
//!
//! This thread has neither constraint. It is an ordinary kernel thread, so it may block, and it
//! drains without a per-pass bound. It runs at BACKGROUND so an idle machine does not pay for it,
//! and donates REALTIME to itself while the backlog is over [`BACKLOG_HIGH`] or memory is low --
//! which is what gets it scheduled promptly on a machine that is busy producing the backlog.

use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::Ordering;

use super::{ThreadRef, current_thread_ref, entry::start_new_kernel, priority::Priority};
use crate::{
    condvar::CondVar,
    once::Once,
    processor::{EXITED_BACKLOG, REAPED, mp::all_processors},
    spinlock::Spinlock,
};

/// Threads awaiting reap before the reaper boosts itself. Each holds a 2 MiB kernel stack, so this
/// is a memory bound wearing a count: 8 is 16 MiB.
const BACKLOG_HIGH: usize = 8;

struct Reaper {
    cv: CondVar,
    /// The condvar needs a lock to wait on; nothing is protected by it.
    lock: Spinlock<()>,
}

static REAPER: Once<Reaper> = Once::new();

pub fn start() {
    REAPER.call_once(|| Reaper {
        cv: CondVar::new(),
        lock: Spinlock::new(()),
    });
    let th = start_new_kernel(Priority::BACKGROUND, reaper_start, 0, "thread-reaper");
    // Printed so a boot log proves the arm it claims to be: an A/B whose treated arm silently
    // failed to start the thread reads exactly like a treatment that did nothing.
    logln!("[reap] reaper thread started (id {})", th.id());
}

/// Wake the reaper if there is anything for it to do.
///
/// Cheap enough to call on every idle-loop pass: one relaxed load, and a signal only when the
/// backlog is non-empty. The idle loop is the right wake source precisely because it covers the
/// case the stattick safe-point test cannot -- a cpu with nothing in user mode to interrupt.
pub fn notify() {
    if EXITED_BACKLOG.load(Ordering::Relaxed) == 0 {
        return;
    }
    if let Some(r) = REAPER.poll() {
        r.cv.signal();
    }
}

extern "C" fn reaper_start() {
    reaper_main()
}

fn reaper_main() -> ! {
    let r = REAPER.wait();
    let me = current_thread_ref().unwrap();
    let mut batch: Vec<ThreadRef> = Vec::new();
    let mut boosted = false;
    loop {
        let urgent = EXITED_BACKLOG.load(Ordering::Relaxed) >= BACKLOG_HIGH
            || crate::memory::tracker::is_low_mem();
        if urgent && !boosted {
            me.donate_priority(Priority::REALTIME);
            boosted = true;
        } else if !urgent && boosted {
            me.remove_donated_priority();
            boosted = false;
        }

        for p in all_processors().iter().flatten() {
            p.drain_exited(&mut batch);
        }

        if batch.is_empty() {
            let guard = r.lock.lock();
            let _ = r.cv.wait(guard);
            continue;
        }

        // Outside every lock: this is the part that can block.
        for th in batch.drain(..) {
            // Safety: the self-reference box is installed once by `schedule_new_thread` and
            // reclaimed exactly once -- here, or by `Processor::cleanup_exited`, and an entry is
            // taken off the list by exactly one of them.
            let _ = unsafe { Box::from_raw(*th.self_reference.get().as_ref().unwrap()) };
            REAPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}
