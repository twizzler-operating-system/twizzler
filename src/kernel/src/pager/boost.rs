//! Priority-following for the pager completion thread.
//!
//! The completion thread is the only thing that can finish a page-in, so every thread blocked on
//! the pager is blocked behind it -- a textbook priority inversion, and one the ordinary donation
//! path in [`crate::thread::priority`] cannot express: donation carries a single value, while the
//! completion thread serves *all* waiters at once and has to run at the highest class among them
//! rather than at whichever one donated last.
//!
//! So waiters are counted per class instead. A thread increments its class's count after
//! submitting and before parking, and drops it on the way out; the completion thread's priority is
//! then just the highest class with a live waiter, floored at [`COMPLETION_BASE`]. That is exact
//! under overlap (two realtime waiters keep the boost until both are gone) and needs no ordering
//! between waiters, which a single donation slot cannot manage.
//!
//! Boost only, never demote below the base: the completion thread serves every waiter, so lowering
//! it for a background waiter would delay page-ins that unrelated threads of higher class are
//! already queued behind. Only the ceiling moves.

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::{
    once::Once,
    thread::{
        ThreadRef, current_thread_ref,
        priority::{Priority, PriorityClass},
    },
};

/// Resting priority of the completion thread.
///
/// It used to start at [`Priority::REALTIME`], which is the inversion the wrong way round: the
/// thread wakes constantly and does very little per wake (`pagerperf`: 3,526 wakes for 62
/// statticks), so at realtime it preempted the compiler it was fetching pages for, on every
/// completion, whether or not anything realtime was waiting. `User` is what it is worth when
/// nothing is blocked on it; the counters below buy back the cases where that is wrong.
pub(super) const COMPLETION_BASE: Priority = Priority::USER;

/// A/B knob. `false` pins the completion thread at [`COMPLETION_BASE`] and stops the counters
/// having any effect, so setting it false *and* `COMPLETION_BASE` to [`Priority::REALTIME`]
/// reproduces the pre-change behaviour exactly. Left as a const rather than removed because the
/// priority half and the spin half of this work are separately attributable, and a measurement
/// that cannot separate them will credit one for the other.
const FOLLOW_WAITER_PRIORITY: bool = true;

const NR_CLASSES: usize = 4;

/// Live waiters by [`PriorityClass`], indexed by `class as usize`.
static WAITERS: [AtomicUsize; NR_CLASSES] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

static COMPLETION: Once<ThreadRef> = Once::new();

/// Waits started, by class, and the number that actually moved the completion thread. Without
/// these the change is unfalsifiable: `raised == 0` means every wait came from the base class and
/// the mechanism never fired, which is a perfectly possible outcome and one that should be
/// reported rather than assumed away.
static WAITS: [AtomicU64; NR_CLASSES] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static RAISED: AtomicU64 = AtomicU64::new(0);

pub(super) fn print_stats() {
    let waits: [u64; NR_CLASSES] = core::array::from_fn(|i| WAITS[i].load(Ordering::Relaxed));
    if waits.iter().all(|w| *w == 0) {
        return;
    }
    logln!(
        "  completion priority: waits by class idle={} bg={} user={} rt={}; {} raised the \
         completion thread (base {:?})",
        waits[0],
        waits[1],
        waits[2],
        waits[3],
        RAISED.load(Ordering::Relaxed),
        COMPLETION_BASE.class,
    );
}

pub(super) fn register_completion_thread(thread: ThreadRef) {
    COMPLETION.call_once(|| thread);
}

fn class_of(idx: usize) -> PriorityClass {
    match idx {
        0 => PriorityClass::Idle,
        1 => PriorityClass::Background,
        2 => PriorityClass::User,
        _ => PriorityClass::Realtime,
    }
}

fn canonical(class: PriorityClass) -> Priority {
    match class {
        PriorityClass::Idle => Priority::IDLE,
        PriorityClass::Background => Priority::BACKGROUND,
        PriorityClass::User => Priority::USER,
        PriorityClass::Realtime => Priority::REALTIME,
    }
}

/// Highest class with a waiter, searched downward so the common answer is found first.
fn highest_waiting() -> Option<PriorityClass> {
    (0..NR_CLASSES)
        .rev()
        .find(|i| WAITERS[*i].load(Ordering::Acquire) != 0)
        .map(class_of)
}

/// Re-derive the completion thread's priority from the counters.
///
/// Deliberately recomputed from scratch on every change rather than adjusted incrementally: two
/// waiters racing here both read the same counters and converge on the same answer, where
/// increment/decrement of a stored priority would not. A lost update leaves the thread one step
/// stale-high, which `set_priority`'s own comment calls the safe direction.
fn recompute() {
    if !FOLLOW_WAITER_PRIORITY {
        return;
    }
    let Some(thread) = COMPLETION.poll() else {
        return;
    };
    let target = match highest_waiting() {
        Some(class) if class > COMPLETION_BASE.class => canonical(class),
        _ => COMPLETION_BASE,
    };
    if thread.base_priority() != target {
        if target > COMPLETION_BASE {
            RAISED.fetch_add(1, Ordering::Relaxed);
        }
        thread.set_priority(target);
    }
}

/// Counts the calling thread as a pager waiter for as long as it is held.
///
/// Taken *after* the request is submitted and before parking, so the boost covers the window the
/// completion thread is actually needed in and not the submit path, which runs on the waiter's own
/// priority anyway.
pub(super) struct WaitBoost {
    idx: usize,
}

impl WaitBoost {
    pub(super) fn new() -> Self {
        let class = current_thread_ref()
            .map(|t| t.effective_priority().class)
            .unwrap_or(PriorityClass::User);
        let idx = class as usize;
        WAITS[idx].fetch_add(1, Ordering::Relaxed);
        WAITERS[idx].fetch_add(1, Ordering::AcqRel);
        // Only a class above the base can change anything, so the common User waiter pays a
        // counter bump and nothing else.
        if class > COMPLETION_BASE.class {
            recompute();
        }
        Self { idx }
    }
}

impl Drop for WaitBoost {
    fn drop(&mut self) {
        WAITERS[self.idx].fetch_sub(1, Ordering::AcqRel);
        if class_of(self.idx) > COMPLETION_BASE.class {
            recompute();
        }
    }
}
