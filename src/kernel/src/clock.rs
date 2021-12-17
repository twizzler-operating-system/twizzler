use core::sync::atomic::{AtomicU64, Ordering};

use alloc::{
    boxed::Box,
};

use crate::{processor::current_processor, spinlock::Spinlock};

pub type Nanoseconds = u64;

pub fn statclock(dt: Nanoseconds) {
    crate::sched::schedule_stattick(dt);
    //logln!("stat {} {}", current_processor().id, dt);
}

const NR_WINDOWS: usize = 1024;

struct Timeout {
    cb: fn(),
    next: Option<Box<Timeout>>,
}
struct TimeoutQueue {
    queues: [Option<Box<Timeout>>; NR_WINDOWS],
    current: usize,
    next_wake: usize,
    soft_current: usize,
}

impl TimeoutQueue {
    const fn new() -> Self {
        const INIT: Option<Box<Timeout>> = None;
        Self {
            queues: [INIT; NR_WINDOWS],
            current: 0,
            next_wake: 0,
            soft_current: 0,
        }
    }

    fn hard_advance(&mut self, ticks: usize) {
        self.current += ticks;
    }

    fn get_next_ticks(&self) -> u64 {
        for i in 1..(NR_WINDOWS - 1) {
            let idx = (i + self.current) % NR_WINDOWS;
            if self.queues[idx].is_some() {
                return i as u64;
            }
        }
        NR_WINDOWS as u64
    }
}

static TIMEOUT_QUEUE: Spinlock<TimeoutQueue> = Spinlock::new(TimeoutQueue::new());

fn ticks_to_nano(ticks: u64) -> Option<Nanoseconds> {
    ticks.checked_mul(1000000)
}

fn nano_to_ticks(ticks: Nanoseconds) -> u64 {
    ticks / 1000000
}

#[thread_local]
static NR_CPU_TICKS: AtomicU64 = AtomicU64::new(0);
#[thread_local]
static NEXT_TICK: AtomicU64 = AtomicU64::new(0);

pub fn schedule_oneshot_tick(next: u64) {
    let time = ticks_to_nano(next).unwrap();
    NEXT_TICK.store(next, Ordering::SeqCst);
    crate::arch::schedule_oneshot_tick(time);
}

pub fn check_reschedule_oneshot() {
    if !current_processor().is_bsp() {
        return;
    }
    crate::interrupt::with_disabled(|| {
        let mut timeout_queue = TIMEOUT_QUEUE.lock();
        let next = timeout_queue.get_next_ticks();
        if next < NEXT_TICK.load(Ordering::SeqCst) {
            timeout_queue.next_wake = next as usize;
            schedule_oneshot_tick(next);
        }
    });
}

pub fn oneshot_clock_hardtick() {
    let ticks = NEXT_TICK.load(Ordering::SeqCst);
    NR_CPU_TICKS.fetch_add(ticks, Ordering::SeqCst);
    let to_next_tick = if current_processor().is_bsp() {
        let mut timeout_queue = TIMEOUT_QUEUE.lock();
        timeout_queue.hard_advance(ticks as usize);
        let next = timeout_queue.get_next_ticks();
        timeout_queue.next_wake = next as usize;
        Some(next)
    } else {
        None
    };

    let sched_next_tick = crate::sched::schedule_hardtick();
    /*
    logln!(
        "hardtick {} {} {:?} {:?}",
        current_processor().id,
        ticks,
        sched_next_tick,
        to_next_tick
    );
    */

    let next = core::cmp::min(
        to_next_tick.unwrap_or(u64::MAX),
        sched_next_tick.unwrap_or(u64::MAX),
    );

    if next != u64::MAX {
        schedule_oneshot_tick(next);
    }
}

pub fn init() {
    crate::arch::start_clock(127, statclock);
}
