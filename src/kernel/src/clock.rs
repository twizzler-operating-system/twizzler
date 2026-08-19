use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use log::{debug, warn};
use twizzler_abi::syscall::{
    Clock, ClockFlags, ClockID, ClockInfo, ClockKind, ClockSource, FemtoSeconds,
};
use twizzler_rt_abi::{Result, error::ArgumentError};

use crate::{
    condvar::CondVar,
    once::Once,
    processor::{
        mp::current_processor,
        sched::{schedule_hardtick, schedule_stattick},
    },
    spinlock::Spinlock,
    syscall::sync::requeue_all,
    thread::{ThreadRef, priority::Priority},
    time::{CLOCK_OFFSET, ClockHardware, TICK_SOURCES, Ticks},
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
    schedule_stattick(dt);
}

const NR_WINDOWS: usize = 1024;

struct TimeoutOnce {
    cb: fn(ThreadRef, u64),
    thread: ThreadRef,
    /// The sleep this callback belongs to. `soft_advance` removes an entry from the queue before
    /// the callback runs, so from that moment `TimeoutKey::release` can no longer stop it; the
    /// callback checks this against the thread's current value instead and does nothing if the
    /// sleep it was registered for has since ended.
    sleep_gen: u64,
}

impl TimeoutOnce {
    fn new(cb: fn(ThreadRef, u64), thread: ThreadRef, sleep_gen: u64) -> Self {
        Self {
            cb,
            thread,
            sleep_gen,
        }
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
        (self.timeout.cb)(self.timeout.thread, self.timeout.sleep_gen)
    }
}

const NR_WINDOW_ENTRIES: usize = 32;
#[derive(Debug)]
struct TimeoutQueue {
    queues: [heapless::Vec<TimeoutEntry, NR_WINDOW_ENTRIES>; NR_WINDOWS],
    /// One bit per window, set while that window has entries.
    ///
    /// [`TimeoutQueue::get_next_ticks`] asks "which is the next non-empty window", and used to
    /// answer it by reading `is_empty()` on up to 1023 of the `queues` themselves. Each window is
    /// a `heapless::Vec` of 32 entries, so consecutive windows are ~1.3 KiB apart: that scan
    /// touched 1023 distinct cache lines spread over 1.3 MiB, on **every hardtick**. This
    /// bitmap is 128 bytes -- two cache lines -- and answers the same question.
    occupied: [u64; NR_WINDOWS / 64],
    current: usize,
    next_wake: usize,
    soft_current: usize,
    keys: heapless::Vec<usize, { NR_WINDOW_ENTRIES * NR_WINDOWS }>,
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
        did_remove
    }
}

impl TimeoutQueue {
    const fn new() -> Self {
        const INIT: heapless::Vec<TimeoutEntry, NR_WINDOW_ENTRIES> = heapless::Vec::new();
        Self {
            queues: [INIT; NR_WINDOWS],
            occupied: [0; NR_WINDOWS / 64],
            current: 0,
            next_wake: 0,
            soft_current: 0,
            keys: heapless::Vec::new(),
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
            if self.keys.push(key).is_err() {
                log::warn!("leaking timeout key {}", key);
            }
        }
    }

    fn hard_advance(&mut self, ticks: usize) {
        let mut wakeup = false;
        for i in 0..(ticks + 1) {
            let window = (self.current + i) % NR_WINDOWS;
            // Via `occupied` rather than the queue, so the tick path touches none of the 1.3 MiB
            // `queues` array.
            if self.occupied[window / 64] & (1u64 << (window % 64)) != 0 {
                wakeup = true;
                break;
            }
        }
        self.current += ticks;
        if wakeup {
            TIMEOUT_THREAD_CONDVAR.signal();
        }
    }

    /// Bring `window`'s bit in [`TimeoutQueue::occupied`] back in line with its queue. Call after
    /// every mutation of `queues[window]`.
    fn sync_occupied(&mut self, window: usize) {
        let (word, bit) = (window / 64, 1u64 << (window % 64));
        if self.queues[window].is_empty() {
            self.occupied[word] &= !bit;
        } else {
            self.occupied[word] |= bit;
        }
    }

    fn get_next_ticks(&self) -> u64 {
        // Nothing pending at all is the common case on a tick, and it costs 16 loads to say so.
        if self.occupied.iter().all(|word| *word == 0) {
            return NR_WINDOWS as u64;
        }
        for i in 1..(NR_WINDOWS - 1) {
            let idx = (i + self.current) % NR_WINDOWS;
            if self.occupied[idx / 64] & (1u64 << (idx % 64)) != 0 {
                return i as u64;
            }
        }
        NR_WINDOWS as u64
    }

    fn insert(&mut self, time: Nanoseconds, timeout: TimeoutOnce) -> TimeoutKey {
        let ticks = nano_to_ticks(time);
        let expire_ticks = self.current + ticks as usize;
        let window = expire_ticks % NR_WINDOWS;
        let key = self.next_key();
        let entry = TimeoutEntry {
            timeout,
            expire_ticks: expire_ticks as u64,
            key,
        };
        if let Err(entry) = self.queues[window].push(entry) {
            log::warn!("timeout queue overflow");
            entry.call();
        }
        self.sync_occupied(window);
        if expire_ticks < self.next_wake {
            // TODO: #41 signal CPU to wake up early.
        }
        TimeoutKey { key, window }
    }

    // Remove a timeout key. Returns true if the key was actually removed (timeout hasn't fired).
    fn remove(&mut self, key: &TimeoutKey) -> bool {
        let old_len = self.queues[key.window].len();
        while let Some(pos) = self.queues[key.window]
            .iter()
            .position(|entry| entry.key == key.key)
        {
            self.queues[key.window].swap_remove(pos);
        }
        self.sync_occupied(key.window);
        self.release_key(key.key);
        // Did we remove anything?
        old_len != self.queues[key.window].len()
    }

    fn check_window(&mut self, window: usize) -> Option<TimeoutEntry> {
        if !self.queues[window].is_empty() {
            let index = self.queues[window]
                .iter()
                .position(|x| x.is_ready(self.current as u64));
            let entry = index.map(|index| self.queues[window].swap_remove(index));
            self.sync_occupied(window);
            return entry;
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
        warn!("timeout thread is blocked");
    }
    debug!("timeout queue: {:?}", *TIMEOUT_QUEUE.lock());
}

fn timeout_thread_set_has_work() {}

/// Register `cb` to fire after `time`. `sleep_gen` is the caller's [`Thread::sync_sleep_gen`] at
/// registration: the callback is handed it back and must ignore the timeout if it has moved on.
pub fn register_timeout_callback(
    time: Nanoseconds,
    cb: fn(ThreadRef, u64),
    thread: ThreadRef,
    sleep_gen: u64,
) -> TimeoutKey {
    let timeout = TimeoutOnce::new(cb, thread, sleep_gen);
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
            let _ = TIMEOUT_THREAD_CONDVAR.wait(tq);
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

/// Watchdog for a stalled BSP, run from *non-BSP* idle loops.
///
/// Every hang diagnostic this kernel has -- `check_timed_out_mutexes`, `check_orphan_threads`,
/// `check_system_hang` -- runs from the `is_bsp()` arm of `idle_main`, and every timeout is
/// advanced from the `is_bsp()` arm of `oneshot_clock_hardtick`. So the one failure they exist to
/// catch, a BSP spinning with interrupts masked, is precisely the one where none of them can run:
/// the BSP never reaches its idle loop, no timeout fires, and the transcript simply stops. That is
/// the `test_mutex` smp4 wedge's undiagnosability, and it is structural rather than bad luck.
///
/// This closes it from the other side, *partly*. A non-BSP cpu that has work keeps taking its own
/// hardticks -- each cpu programs and rearms its own LAPIC one-shot -- so it keeps reaching its
/// idle loop and can notice that `BSP_TICK` has stopped moving. Costs one relaxed load per caller
/// in the normal case, and reports once per boot.
///
/// **Where it does not reach.** An *idle* cpu is halted, and what wakes it is largely the statclock
/// -- which the PIT delivers to the BSP alone, the BSP re-broadcasting it by IPI (see the
/// TIMER_VECTOR arm in `arch/amd64/interrupt.rs`). So a BSP that stops taking interrupts also stops
/// waking the cpus that are supposed to report on it, and they never run this. That is not
/// hypothetical: across 46 captured wedges this fired zero times. Treat a silent watchdog as no
/// evidence either way, and note that the wedges it did not catch had a BSP spinning with
/// interrupts *on* -- ticking normally, which this cannot see by construction.
pub mod bsp_watchdog {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::BSP_TICK;

    /// Idle-loop observations of an unchanged `BSP_TICK` before we call it stalled. The caller runs
    /// this every 1000 idle iterations, each of which halts until an interrupt, so this is
    /// deliberately generous: the cost of being late is a slower report, the cost of being early is
    /// a false alarm in a transcript someone will read as a bug.
    const STALL_OBSERVATIONS: u64 = 16;

    static LAST_SEEN: AtomicU64 = AtomicU64::new(0);
    static UNCHANGED: AtomicU64 = AtomicU64::new(0);
    static FIRED: AtomicBool = AtomicBool::new(false);

    /// True once, when the BSP's tick has stopped advancing for long enough to be a wedge.
    pub fn stalled() -> bool {
        let now = BSP_TICK.load(Ordering::Relaxed);
        let last = LAST_SEEN.swap(now, Ordering::Relaxed);
        if now != last {
            UNCHANGED.store(0, Ordering::Relaxed);
            return false;
        }
        // Before the first tick there is nothing to have stalled.
        if now == 0 {
            return false;
        }
        let n = UNCHANGED.fetch_add(1, Ordering::Relaxed) + 1;
        n >= STALL_OBSERVATIONS && !FIRED.swap(true, Ordering::Relaxed)
    }
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

    // Backstop for the requeue list. A waker can only park a thread there when it can't schedule
    // it directly (the target is critical, or hasn't set sync_sleep_done yet), and the waker's own
    // requeue_all() skips it for the same reason. The sleeper's claim_own_wakeup() covers the
    // common case, but draining every hardtick bounds how long anything can sit there if some
    // path doesn't -- without it, a single missed drain stalls that thread indefinitely.
    requeue_all();
    let mut sched_next_tick = schedule_hardtick();
    if current_processor().is_bsp() {
        sched_next_tick = Some(1);
    }
    log::trace!(
        "hardtick {} {} {:?} {:?}",
        current_processor().id,
        ticks,
        sched_next_tick,
        to_next_tick
    );
    let next = core::cmp::min(
        to_next_tick.unwrap_or(u64::MAX),
        sched_next_tick.unwrap_or(u64::MAX),
    );

    // Always rearm. The timer is one-shot, so a cpu that leaves a hardtick without programming the
    // next one takes no further timer interrupt for the rest of the boot -- it keeps running, and
    // keeps answering IPIs, so nothing looks wrong until you notice its timeslices never expire.
    //
    // The bsp is already guarded against this (`sched_next_tick = Some(1)` above, unconditionally).
    // No other cpu was: `to_next_tick` is `None` off-bsp by construction, so a non-bsp cpu that
    // takes one hardtick while `schedule_hardtick` returns `None` -- which is any hardtick with no
    // current thread -- retires its own clock permanently. One tick is all it costs.
    let next = if next == u64::MAX { REARM_TICKS } else { next };
    schedule_oneshot_tick(next);
}

/// Fallback rearm interval, in ticks (milliseconds), when nothing else asked for one.
const REARM_TICKS: u64 = 10;

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
            // Only slots that actually hold a clock. This used to list every slot up to
            // `MAX_CLOCKS` unconditionally, which was harmless only because `register_clock` had
            // a matching bug that filled all of them with copies of the first clock. With that
            // fixed the unregistered slots are `None`, and the `fill_with_*` readers below
            // `unwrap()` what they find -- so listing an empty slot here is a kernel panic
            // reachable by `sys_read_clock_list`. The two bugs concealed each other.
            {
                let sources = TICK_SOURCES.lock();
                for (i, source) in sources.iter().enumerate().skip(CLOCK_OFFSET) {
                    if source.is_some() {
                        clock_vec.push(ClockID(i as u64));
                    }
                }
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

/// Resolve a user-supplied [`ClockSource`] to a tick-source index and the flags to report.
///
/// Split out of the syscall handler so the bounds check has somewhere to be tested. The handler
/// checked `src as usize > clock_list.len()` -- off by one -- and then indexed the array, so
/// `ClockSource::ID(ClockID(MAX_CLOCKS))` read one past the end of an eight-element array and
/// panicked the kernel from unprivileged userspace.
pub fn resolve_clock_source(source: ClockSource) -> Result<(usize, ClockFlags)> {
    match source {
        ClockSource::BestMonotonic => Ok((0, ClockFlags::MONOTONIC)),
        ClockSource::BestRealTime => Ok((1, ClockFlags::empty())),
        ClockSource::ID(id) => {
            let idx: usize = id.0.try_into().map_err(|_| ArgumentError::InvalidArgument)?;
            if idx >= crate::time::MAX_CLOCKS {
                return Err(ArgumentError::InvalidArgument.into());
            }
            Ok((idx, ClockFlags::empty()))
        }
    }
}

/// A registered clock's info, or `None` if that slot holds no clock.
///
/// Every caller here used to index `TICK_SOURCES` and `unwrap()`, so a clock id naming an empty
/// slot took the kernel down rather than returning an error.
fn clock_info(id: ClockID) -> Option<ClockInfo> {
    let idx: usize = id.0.try_into().ok()?;
    Some(TICK_SOURCES.lock().get(idx)?.as_ref()?.info())
}

// A list of user clocks that are exposed to user space
static USER_CLOCKS: Spinlock<Vec<Vec<ClockID>>> = Spinlock::new(Vec::new());
static mut CLOCK_LEN: usize = 0;

// fills the passed in slice with the first clock from each clock list
pub fn fill_with_every_first(slice: &mut [Clock], start: u64) -> Result<usize> {
    // error check bounds of start
    // there are currently only 3 kinds of clocks exposed
    if start >= 3 {
        // index out of bounds
        return Err(ArgumentError::InvalidArgument.into());
    }

    let mut clocks_added = 0;
    // determine what clock list we need to be in
    for (i, clock_list) in USER_CLOCKS.lock()[start as usize..].iter().enumerate() {
        // add first clock in this list to the user slice
        // check that we don't go out of slice bounds
        if clocks_added < slice.len() {
            // does this allocate new kernel memory?
            let Some(id) = clock_list.first().copied() else {
                continue;
            };
            let Some(info) = clock_info(id) else {
                continue;
            };
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
pub fn fill_with_kind(slice: &mut [Clock], clock: ClockKind, start: u64) -> Result<usize> {
    // determine what clock list we need to be in
    let i: u64 = clock.into();
    let clock_list = &USER_CLOCKS.lock()[i as usize];
    // error check bounds of start
    if start as usize >= clock_list.len() {
        // index out of bounds
        return Err(ArgumentError::InvalidArgument.into());
    }
    let mut clocks_added = 0;
    // add each clock in this list to the user slice
    for id in &clock_list[start as usize..] {
        // check that we don't go out of slice bounds
        if clocks_added < slice.len() {
            let Some(info) = clock_info(*id) else {
                continue;
            };
            slice[clocks_added].set(info, *id, clock);
            clocks_added += 1;
        } else {
            break;
        }
    }
    return Ok(clocks_added);
}

// fils the passed in slice with the first element of a specific clock type
pub fn fill_with_first_kind(slice: &mut [Clock], clock: ClockKind) -> Result<usize> {
    // determine what clock list we need to be in
    let i: u64 = clock.into();
    let clock_list = &USER_CLOCKS.lock()[i as usize];
    let clocks_added = 1;
    // check that we don't go out of slice bounds
    if slice.len() >= 1 {
        let id = clock_list.first().ok_or(ArgumentError::InvalidArgument)?;
        let info = clock_info(*id).ok_or(ArgumentError::InvalidArgument)?;
        slice[0].set(info, *id, clock);
        return Ok(clocks_added);
    } else {
        return Err(ArgumentError::InvalidArgument.into());
    }
}

pub fn init() {
    enumerate_hw_clocks();
    materialize_sw_clocks();
    crate::arch::start_clock(127, statclock);
    TIMEOUT_THREAD.call_once(|| {
        crate::thread::entry::start_new_kernel(Priority::INTERRUPT, soft_timeout_clock, 0)
    });
}

#[cfg(test)]
mod tests {
    use twizzler_abi::syscall::{ClockID, ClockSource};
    use twizzler_kernel_macros::kernel_test;

    use super::*;

    /// The bounds check behind `sys_read_clock_info`. Exercised here rather than through the
    /// syscall because a kernel test has no user memory context to hand the ABI wrapper.
    #[kernel_test]
    fn test_resolve_clock_source_rejects_out_of_range() {
        for id in [
            crate::time::MAX_CLOCKS as u64,
            crate::time::MAX_CLOCKS as u64 + 1,
            u64::MAX,
        ] {
            assert!(
                resolve_clock_source(ClockSource::ID(ClockID(id))).is_err(),
                "clock id {} was accepted",
                id
            );
        }
        // ...and did not simply start rejecting everything.
        assert!(resolve_clock_source(ClockSource::BestMonotonic).is_ok());
        assert!(resolve_clock_source(ClockSource::BestRealTime).is_ok());
        assert!(resolve_clock_source(ClockSource::ID(ClockID(0))).is_ok());
    }

    /// Every clock the enumeration hands out must be readable.
    ///
    /// `register_clock` used to fill all `MAX_CLOCKS` slots with copies of the first clock, and
    /// the enumeration listed every slot up to that bound. The two bugs concealed each other:
    /// fixing either alone leaves the list naming empty slots, which the readers `unwrap()`.
    #[kernel_test]
    fn test_listed_clocks_are_all_readable() {
        let lists = USER_CLOCKS.lock().clone();
        let mut seen = 0;
        for list in &lists {
            for id in list {
                assert!(
                    clock_info(*id).is_some(),
                    "clock list names id {} with no registered source",
                    id.0
                );
                seen += 1;
            }
        }
        assert!(seen > 0, "no clocks were enumerated at all");
    }

    /// The cached read path must agree with the list and refuse out-of-range indices.
    #[kernel_test]
    fn test_read_clock_bounds() {
        assert!(crate::time::read_clock(0).is_some());
        assert!(crate::time::read_clock(crate::time::MAX_CLOCKS).is_none());
        assert!(crate::time::read_clock(usize::MAX).is_none());
    }
}
