use core::sync::atomic::{AtomicU64, Ordering};

use alloc::{boxed::Box, vec::Vec};

use crate::{
    condvar::CondVar,
    once::Once,
    processor::current_processor,
    spinlock::Spinlock,
    thread::{Priority, ThreadRef},
    time::{Ticks, ClockHardware},
};

use twizzler_abi::syscall::{ClockInfo, FemtoSeconds};

pub type Nanoseconds = u64;

pub fn statclock(dt: Nanoseconds) {
    crate::sched::schedule_stattick(dt);
}

const NR_WINDOWS: usize = 1024;

struct TimeoutOnce<T: Send, F: FnOnce(T)> {
    cb: F,
    data: T,
}

impl<T: Send, F: FnOnce(T)> TimeoutOnce<T, F> {
    fn new(cb: F, data: T) -> Self {
        Self { cb, data }
    }
}

trait Timeout {
    fn call(self: Box<Self>);
}

impl<T: Send, F: FnOnce(T)> Timeout for TimeoutOnce<T, F> {
    fn call(self: Box<Self>) {
        (self.cb)(self.data)
    }
}

struct TimeoutEntry {
    timeout: Box<dyn Timeout + Send>,
    expire_ticks: u64,
}

impl core::fmt::Debug for TimeoutEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TimeoutEntry")
            .field("expire_ticks", &self.expire_ticks)
            .finish()
    }
}

impl TimeoutEntry {
    fn is_ready(&self, cur: u64) -> bool {
        cur >= self.expire_ticks
    }

    fn call(self) {
        self.timeout.call()
    }
}

#[derive(Debug)]
struct TimeoutQueue {
    queues: [Vec<TimeoutEntry>; NR_WINDOWS],
    current: usize,
    next_wake: usize,
    soft_current: usize,
}

impl TimeoutQueue {
    const fn new() -> Self {
        const INIT: Vec<TimeoutEntry> = Vec::new();
        Self {
            queues: [INIT; NR_WINDOWS],
            current: 0,
            next_wake: 0,
            soft_current: 0,
        }
    }

    fn hard_advance(&mut self, ticks: usize) {
        let mut wakeup = false;
        for i in 0..(ticks + 1) {
            let window = (self.current + i) % NR_WINDOWS;
            if !self.queues[window].is_empty() {
                wakeup = true;
                break;
            }
        }
        self.current += ticks;
        if wakeup {
            TIMEOUT_THREAD_CONDVAR.signal();
        }
    }

    fn get_next_ticks(&self) -> u64 {
        for i in 1..(NR_WINDOWS - 1) {
            let idx = (i + self.current) % NR_WINDOWS;
            if !self.queues[idx].is_empty() {
                return i as u64;
            }
        }
        NR_WINDOWS as u64
    }

    fn insert(&mut self, time: Nanoseconds, timeout: Box<dyn Timeout + Send>) {
        let ticks = nano_to_ticks(time);
        let expire_ticks = self.current + ticks as usize;
        let window = expire_ticks % NR_WINDOWS;
        self.queues[window].push(TimeoutEntry {
            timeout,
            expire_ticks: expire_ticks as u64,
        });
        if expire_ticks < self.next_wake {
            // TODO: #41 signal CPU to wake up early.
        }
    }
    fn check_window(&mut self, window: usize) -> Option<TimeoutEntry> {
        if !self.queues[window].is_empty() {
            let index = self.queues[window]
                .iter()
                .position(|x| x.is_ready(self.current as u64));
            return index.map(|index| self.queues[window].remove(index));
        }
        None
    }

    fn soft_advance(&mut self) -> Option<TimeoutEntry> {
        while self.soft_current < self.current {
            let window = self.soft_current % NR_WINDOWS;
            if let Some(t) = self.check_window(window) {
                return Some(t);
            }
            self.soft_current += 1;
        }
        let window = self.soft_current % NR_WINDOWS;
        self.check_window(window)
    }
}

static TIMEOUT_QUEUE: Spinlock<TimeoutQueue> = Spinlock::new(TimeoutQueue::new());
static TIMEOUT_THREAD: Once<ThreadRef> = Once::new();
static TIMEOUT_THREAD_CONDVAR: CondVar = CondVar::new();

pub fn print_info() {
    if TIMEOUT_THREAD_CONDVAR.has_waiters() {
        logln!("timeout thread is blocked");
    }
    logln!("timeout queue: {:?}", *TIMEOUT_QUEUE.lock());
}

fn timeout_thread_set_has_work() {}

pub fn register_timeout_callback<T: 'static + Send, F: FnOnce(T) + Send + 'static>(
    time: Nanoseconds,
    cb: F,
    data: T,
) {
    let timeout = TimeoutOnce::new(cb, data);
    TIMEOUT_QUEUE.lock().insert(time, Box::new(timeout));
}

extern "C" fn soft_timeout_clock() {
    /* TODO: use some heuristic to decide if we need to spend more time handling timeouts */
    loop {
        let mut tq = TIMEOUT_QUEUE.lock();
        let timeout = tq.soft_advance();
        if let Some(timeout) = timeout {
            drop(tq);
            timeout.call();
        } else {
            TIMEOUT_THREAD_CONDVAR.wait(tq);
        }
    }
}

// TODO: we could make Nanoseconds an actual type, and Ticks, and then make type-safe conversions
// between them.
pub fn ticks_to_nano(ticks: u64) -> Option<Nanoseconds> {
    ticks.checked_mul(1000000)
}

fn nano_to_ticks(ticks: Nanoseconds) -> u64 {
    ticks / 1000000
}

#[thread_local]
static NR_CPU_TICKS: AtomicU64 = AtomicU64::new(0);
#[thread_local]
static NEXT_TICK: AtomicU64 = AtomicU64::new(0);

static BSP_TICK: AtomicU64 = AtomicU64::new(0);

pub fn get_current_ticks() -> u64 {
    // TODO: something real
    BSP_TICK.load(Ordering::SeqCst)
}

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
        BSP_TICK.fetch_add(ticks, Ordering::SeqCst);
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

fn enumerate_hw_clocks() {
    crate::arch::processor::enumerate_clocks();
    crate::time::register_clock(SoftClockTick {});
    crate::machine::enumerate_clocks();
}

pub struct SoftClockTick;
impl ClockHardware for SoftClockTick {
    fn read(&self) -> Ticks {
        Ticks { value: get_current_ticks(), rate: FemtoSeconds(0)}
    }

    fn info(&self) -> ClockInfo {
        ClockInfo::ZERO
    }
}

pub fn init() {
    enumerate_hw_clocks();
    crate::arch::start_clock(127, statclock);
    TIMEOUT_THREAD
        .call_once(|| crate::thread::start_new_kernel(Priority::REALTIME, soft_timeout_clock));
}
