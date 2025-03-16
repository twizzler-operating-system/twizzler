use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use arrayvec::ArrayVec;
use twizzler_abi::syscall::{
    Clock, ClockID, ClockInfo, ClockKind, FemtoSeconds, ReadClockListError,
};

use crate::{
    condvar::CondVar,
    once::Once,
    processor::current_processor,
    spinlock::Spinlock,
    syscall::sync::requeue_all,
    thread::{priority::Priority, ThreadRef},
    time::{ClockHardware, Ticks, CLOCK_OFFSET, TICK_SOURCES},
};

// TODO: replace with NanoSeconds from twizzler-abi.
pub type Nanoseconds = u64;

// TODO: remove when replacing Nanoseconds.
impl From<Ticks> for Nanoseconds {
    fn from(t: Ticks) -> Self {
        t.value * (t.rate.0 / 1000000)
    }
}

pub fn statclock(dt: Nanoseconds) {
    crate::sched::schedule_stattick(dt);
}

const NR_WINDOWS: usize = 1024;
const NR_SLOTS: usize = 1024;

struct TimeoutOnce {
    cb: fn(Option<ThreadRef>, usize),
    data: usize,
    th: Option<ThreadRef>,
}

impl TimeoutOnce {
    fn new(cb: fn(Option<ThreadRef>, usize), th: Option<ThreadRef>, data: usize) -> Self {
        Self { cb, data, th }
    }

    fn call(self) {
        (self.cb)(self.th, self.data)
    }
}

struct TimeoutEntry {
    timeout: TimeoutOnce,
    expire_ticks: u64,
    key: usize,
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
    queues: [ArrayVec<TimeoutEntry, NR_SLOTS>; NR_WINDOWS],
    current: usize,
    next_wake: usize,
    soft_current: usize,
    keys: ArrayVec<usize, NR_SLOTS>,
    next_key: usize,
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Clone)]
pub struct TimeoutKey {
    key: usize,
    window: usize,
}

impl TimeoutKey {
    /// Remove all timeouts with this key. Returns true if a key was actually removed (timeout
    /// hasn't fired).
    pub fn release(self) -> bool {
        let did_remove = TIMEOUT_QUEUE.lock().remove(&self);
        // Our destructor just calls remove, above, so skip it when doing this manual release.
        core::mem::forget(self);
        did_remove
    }
}

impl Drop for TimeoutKey {
    fn drop(&mut self) {
        TIMEOUT_QUEUE.lock().remove(self);
    }
}

impl TimeoutQueue {
    const fn new() -> Self {
        const INIT: ArrayVec<TimeoutEntry, NR_SLOTS> = ArrayVec::new_const();
        Self {
            queues: [INIT; NR_WINDOWS],
            current: 0,
            next_wake: 0,
            soft_current: 0,
            keys: ArrayVec::new_const(),
            next_key: 0,
        }
    }

    fn next_key(&mut self) -> usize {
        match self.keys.pop() {
            Some(key) => key,
            None => {
                self.next_key += 1;
                self.next_key
            }
        }
    }

    fn release_key(&mut self, key: usize) {
        if key == self.next_key {
            self.next_key -= 1;
        } else {
            self.keys.push(key);
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

    fn insert(&mut self, time: Nanoseconds, timeout: TimeoutOnce) -> Option<TimeoutKey> {
        let ticks = nano_to_ticks(time);
        let expire_ticks = self.current + ticks as usize;
        let window = expire_ticks % NR_WINDOWS;
        let key = self.next_key();
        let entry = TimeoutEntry {
            timeout,
            expire_ticks: expire_ticks as u64,
            key,
        };
        if self.queues[window].try_push(entry).is_err() {
            self.release_key(key);
            return None;
        }
        if expire_ticks < self.next_wake {
            // TODO: #41 signal CPU to wake up early.
        }
        Some(TimeoutKey { key, window })
    }

    // Remove a timeout key. Returns true if the key was actually removed (timeout hasn't fired).
    fn remove(&mut self, key: &TimeoutKey) -> bool {
        let old_len = self.queues[key.window].len();
        self.queues[key.window].retain(|entry| entry.key != key.key);
        self.release_key(key.key);
        // Did we remove anything?
        old_len != self.queues[key.window].len()
    }

    fn check_window(&mut self, window: usize) -> Option<TimeoutEntry> {
        if !self.queues[window].is_empty() {
            let index = self.queues[window]
                .iter()
                .position(|x| x.is_ready(self.current as u64));
            return index.map(|index| self.queues[window].swap_remove(index));
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

pub fn register_timeout_callback(
    time: Nanoseconds,
    cb: fn(Option<ThreadRef>, usize),
    thread: Option<ThreadRef>,
    data: usize,
) -> Option<TimeoutKey> {
    let timeout = TimeoutOnce::new(cb, thread, data);
    TIMEOUT_QUEUE.lock().insert(time, timeout)
}

extern "C" fn soft_timeout_clock() {
    /* TODO: use some heuristic to decide if we need to spend more time handling timeouts */
    loop {
        let mut tq = TIMEOUT_QUEUE.lock();
        let timeout = tq.soft_advance();
        if let Some(timeout) = timeout {
            drop(tq);
            timeout.call();
            requeue_all();
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

// create clocks exposed to userspace
fn materialize_sw_clocks() {
    // in the future we will do something a bit more clever
    // that will take into account the properties of the hardware
    // to map to a semantic clock type
    organize_clock_sources(ClockKind::Monotonic);
    organize_clock_sources(ClockKind::RealTime);
    organize_clock_sources(ClockKind::Unknown);
}

fn organize_clock_sources(kind: ClockKind) {
    // 0 at this time maps to a monotonic clock source
    // which at this time is sufficient for the monotonic
    // and real-time user clocks
    match kind {
        ClockKind::Monotonic => {
            let mut clock_vec = Vec::new();
            clock_vec.push(ClockID(0));
            USER_CLOCKS.lock().push(clock_vec);
        }
        ClockKind::RealTime => {
            let mut clock_vec = Vec::new();
            clock_vec.push(ClockID(0));
            USER_CLOCKS.lock().push(clock_vec);
        }
        ClockKind::Unknown => {
            // contains every single clock source
            // which could be used for anything
            let mut clock_vec = Vec::new();
            // nothing special here, just a bunch of integers
            // representing the clock ids of the TICK_SOURCES
            let num_clocks: u64 = { TICK_SOURCES.lock().len() }.try_into().unwrap();
            for i in CLOCK_OFFSET as u64..num_clocks {
                clock_vec.push(ClockID(i));
            }
            USER_CLOCKS.lock().push(clock_vec)
        }
    }
}

pub struct SoftClockTick;
impl ClockHardware for SoftClockTick {
    fn read(&self) -> Ticks {
        Ticks {
            value: get_current_ticks(),
            rate: FemtoSeconds(0),
        }
    }

    fn info(&self) -> ClockInfo {
        ClockInfo::ZERO
    }
}

// A list of user clocks that are exposed to user space
static USER_CLOCKS: Spinlock<Vec<Vec<ClockID>>> = Spinlock::new(Vec::new());
static mut CLOCK_LEN: usize = 0;

// fills the passed in slice with the first clock from each clock list
pub fn fill_with_every_first(slice: &mut [Clock], start: u64) -> Result<usize, ReadClockListError> {
    // error check bounds of start
    // there are currently only 3 kinds of clocks exposed
    if start >= 3 {
        // index out of bounds
        return Err(ReadClockListError::InvalidArgument);
    }

    let mut clocks_added = 0;
    // determine what clock list we need to be in
    for (i, clock_list) in USER_CLOCKS.lock()[start as usize..].iter().enumerate() {
        // add first clock in this list to the user slice
        // check that we don't go out of slice bounds
        if clocks_added < slice.len() {
            // does this allocate new kernel memory?
            let info = { TICK_SOURCES.lock()[clock_list.first().unwrap().0 as usize].info() };
            slice[clocks_added].set(
                // each semantic clock will have at least one element
                info,
                clock_list[0],
                (i as u64).into(),
            );
            clocks_added += 1;
        } else {
            break;
        }
    }
    return Ok(clocks_added);
}

// fills the passed in slice with all clocks from a specified clock list
pub fn fill_with_kind(
    slice: &mut [Clock],
    clock: ClockKind,
    start: u64,
) -> Result<usize, ReadClockListError> {
    // determine what clock list we need to be in
    let i: u64 = clock.into();
    let clock_list = &USER_CLOCKS.lock()[i as usize];
    // error check bounds of start
    if start as usize >= clock_list.len() {
        // index out of bounds
        return Err(ReadClockListError::InvalidArgument);
    }
    let mut clocks_added = 0;
    // add each clock in this list to the user slice
    for id in &clock_list[start as usize..] {
        // check that we don't go out of slice bounds
        if clocks_added < slice.len() {
            let info = { TICK_SOURCES.lock()[id.0 as usize].info() };
            slice[clocks_added].set(info, *id, clock);
            clocks_added += 1;
        } else {
            break;
        }
    }
    return Ok(clocks_added);
}

// fils the passed in slice with the first element of a specific clock type
pub fn fill_with_first_kind(
    slice: &mut [Clock],
    clock: ClockKind,
) -> Result<usize, ReadClockListError> {
    // determine what clock list we need to be in
    let i: u64 = clock.into();
    let clock_list = &USER_CLOCKS.lock()[i as usize];
    let clocks_added = 1;
    // check that we don't go out of slice bounds
    if slice.len() >= 1 {
        let id = clock_list.first().unwrap();
        let info = { TICK_SOURCES.lock()[id.0 as usize].info() };
        slice[0].set(info, *id, clock);
        return Ok(clocks_added);
    } else {
        return Err(ReadClockListError::InvalidArgument);
    }
}

pub fn init() {
    enumerate_hw_clocks();
    materialize_sw_clocks();
    crate::arch::start_clock(127, statclock);
    TIMEOUT_THREAD.call_once(|| {
        crate::thread::entry::start_new_kernel(Priority::REALTIME, soft_timeout_clock, 0)
    });
}
