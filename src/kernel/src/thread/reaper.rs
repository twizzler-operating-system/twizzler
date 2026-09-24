//! Reaping exited threads.
//!
//! A thread cannot drop its own last reference -- it is still running on the kernel stack that drop
//! hands back to the free list -- so the drop is deferred to another context. The idle loop reaps
//! one per hundred passes, and blocking on a teardown lock from an idle thread is the wedge
//! `schedule` documents at length -- an idle thread descheduled holding a mutex is a lock owner
//! nothing can schedule. (`schedule_stattick` used to reap too, from the timer interrupt: a
//! teardown that blocked there switched away with the timer not yet re-armed.)
//!
//! This thread has neither constraint. It is an ordinary kernel thread, so it may block, and it
//! drains without a per-pass bound. It runs at BACKGROUND so an idle machine does not pay for it,
//! and at REALTIME while [`boost_wanted`] says so. A BACKGROUND thread never gets a cpu that user
//! threads keep busy, so the boost cannot wait for the reaper to notice: [`notify`] donates it from
//! the exit path.

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

/// Past [`BACKLOG_HIGH`], or with anything at all to reap while memory is low: every exited thread
/// pins a kernel stack, and kernel-stack refill panics rather than waits when the heap cannot grow,
/// so under pressure a spawn storm must not outrun the reaper.
fn boost_wanted(backlog: usize) -> bool {
    backlog >= BACKLOG_HIGH || (backlog > 0 && crate::memory::tracker::is_low_mem())
}

struct Reaper {
    cv: CondVar,
    /// The condvar needs a lock to wait on; nothing is protected by it.
    lock: Spinlock<()>,
}

static REAPER: Once<Reaper> = Once::new();
static THREAD: Once<ThreadRef> = Once::new();

pub fn start() {
    REAPER.call_once(|| Reaper {
        cv: CondVar::new(),
        lock: Spinlock::new(()),
    });
    let th = start_new_kernel(Priority::BACKGROUND, reaper_start, 0, "thread-reaper");
    // Printed so a boot log proves the arm it claims to be: an A/B whose treated arm silently
    // failed to start the thread reads exactly like a treatment that did nothing.
    logln!("[reap] reaper thread started (id {})", th.id());
    THREAD.call_once(|| th);
}

/// Wake the reaper if there is anything for it to do, boosting it per [`boost_wanted`].
///
/// Cheap enough for every idle-loop pass and every exit: one relaxed load, and a signal only
/// when the backlog is non-empty.
pub fn notify() {
    let backlog = EXITED_BACKLOG.load(Ordering::Relaxed);
    if backlog == 0 {
        return;
    }
    if let Some(r) = REAPER.poll() {
        if boost_wanted(backlog) {
            if let Some(th) = THREAD.poll() {
                th.donate_priority(Priority::REALTIME);
            }
        }
        r.cv.signal();
    }
}

/// Reap every exited thread that is ready for it, on the calling thread.
///
/// Same work as one pass of [`reaper_main`], and safe from an ordinary syscall thread for the same
/// two reasons the reaper is: it may block (`Thread::drop` -> `IdCounter::release` takes a
/// *sleeping* mutex), and `drain_exited` hands out each entry exactly once under its lock, so the
/// self-reference box is still reclaimed exactly once. It also cannot reap the caller: a thread
/// that has not left its kernel stack is skipped there, and the caller is standing on its own.
///
/// Returns the number reaped.
pub fn drain_now() -> usize {
    let mut batch: Vec<ThreadRef> = Vec::new();
    for p in all_processors().iter().flatten() {
        p.drain_exited(&mut batch);
    }
    let n = batch.len();
    for th in batch.drain(..) {
        // Safety: as in `reaper_main` -- the box is installed once by `schedule_new_thread` and
        // reclaimed by whichever of this and `Processor::cleanup_exited` took the entry off the
        // list, which is exactly one of them.
        let _ = unsafe { Box::from_raw(*th.self_reference.get().as_ref().unwrap()) };
        REAPED.fetch_add(1, Ordering::Relaxed);
    }
    n
}

extern "C" fn reaper_start() {
    reaper_main()
}

fn reaper_main() -> ! {
    let r = REAPER.wait();
    let me = current_thread_ref().unwrap();
    let mut batch: Vec<ThreadRef> = Vec::new();
    loop {
        let urgent = boost_wanted(EXITED_BACKLOG.load(Ordering::Relaxed));
        // Read back rather than tracked: `notify` donates from the exit path too.
        let boosted = me.get_donated_priority().is_some();
        if urgent && !boosted {
            me.donate_priority(Priority::REALTIME);
        } else if !urgent && boosted {
            me.remove_donated_priority();
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
