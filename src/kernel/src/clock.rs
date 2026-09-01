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
    /// Absolute deadline on the bench clock. Firing is decided by this, not by the wheel:
    /// window placement only schedules the *visit* (tick-coarse); this carries the sub-tick
    /// remainder. 0 when no clock was registered at insert (early boot) — due on visit.
    expire_ns: u64,
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
    fn is_ready(&self, now_ns: u64) -> bool {
        now_ns >= self.expire_ns
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
    /// [`TimeoutQueue::next_wake_delta_ns`] asks "which is the next non-empty window", and used to
    /// answer it by reading `is_empty()` on up to 1023 of the `queues` themselves. Each window is
    /// a `heapless::Vec` of 32 entries, so consecutive windows are ~1.3 KiB apart: that scan
    /// touched 1023 distinct cache lines spread over 1.3 MiB, on **every hardtick**. This
    /// bitmap is 128 bytes -- two cache lines -- and answers the same question.
    occupied: [u64; NR_WINDOWS / 64],
    current: usize,
    /// Absolute ns deadline of the wake currently programmed on the bsp's oneshot; inserts
    /// compare against this to decide whether to pull the wake in (the old TODO #41).
    next_wake_abs_ns: u64,
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
            next_wake_abs_ns: u64::MAX,
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

    /// Delta ns from `now_ns` to the next wake this queue needs; `None` when empty, `Some(0)`
    /// when an entry is already due. Within the head windows (this tick and the next) the answer
    /// is the entries' actual min deadline, which is what makes sub-tick timeouts fire on time;
    /// farther out it is whole ticks, deliberately one short so the arrival tick can refine.
    /// With no clock yet (`now_ns == 0`) everything is whole ticks.
    fn next_wake_delta_ns(&self, now_ns: u64) -> Option<Nanoseconds> {
        // Nothing pending at all is the common case on a tick, and it costs 16 loads to say so.
        if self.occupied.iter().all(|word| *word == 0) {
            return None;
        }
        for i in 0..NR_WINDOWS {
            let idx = (i + self.current) % NR_WINDOWS;
            if self.occupied[idx / 64] & (1u64 << (idx % 64)) == 0 {
                continue;
            }
            if i <= 1 && now_ns != 0 {
                // A head window can also hold next-revolution entries; their deadlines are at
                // least a full revolution out, so taking the min stays correct.
                let min = self.queues[idx].iter().map(|e| e.expire_ns).min().unwrap();
                return Some(min.saturating_sub(now_ns));
            }
            return Some(ticks_to_nano((i as u64).saturating_sub(1).max(1)).unwrap());
        }
        None
    }

    fn insert(&mut self, time: Nanoseconds, timeout: TimeoutOnce) -> (TimeoutKey, u64) {
        let now = crate::instant::current_ns();
        // Ceil placement: the wheel must never *owe* a window a visit before its whole-tick
        // deadlines pass; the sub-tick remainder is expire_ns's job, not the window's.
        let ticks = nano_to_ticks_ceil(time);
        let expire_ticks = self.current + ticks as usize;
        let window = expire_ticks % NR_WINDOWS;
        let expire_ns = if now == 0 {
            0
        } else {
            now.saturating_add(time)
        };
        let key = self.next_key();
        let entry = TimeoutEntry {
            timeout,
            expire_ticks: expire_ticks as u64,
            expire_ns,
            key,
        };
        if let Err(entry) = self.queues[window].push(entry) {
            log::warn!("timeout queue overflow");
            entry.call();
        }
        self.sync_occupied(window);
        (TimeoutKey { key, window }, expire_ns)
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

    fn check_window(&mut self, window: usize, now_ns: u64) -> Option<TimeoutEntry> {
        if !self.queues[window].is_empty() {
            let index = self.queues[window].iter().position(|x| x.is_ready(now_ns));
            let entry = index.map(|index| self.queues[window].swap_remove(index));
            self.sync_occupied(window);
            return entry;
        }
        None
    }

    /// Move this-revolution entries the wheel is about to walk past that have not reached their
    /// ns deadline (a ceil-placed window can be visited up to one insert-carry early) to just
    /// ahead of the head, where `soft_advance`'s peek and the oneshot refinement can still reach
    /// them. Next-revolution entries (`expire_ticks > current`) stay — their window recurs.
    fn rehome_undue(&mut self, window: usize) {
        let mut i = 0;
        while i < self.queues[window].len() {
            if self.queues[window][i].expire_ticks > self.current as u64 {
                i += 1;
                continue;
            }
            let mut entry = self.queues[window].swap_remove(i);
            entry.expire_ticks = (self.current + 1) as u64;
            let dest = (self.current + 1) % NR_WINDOWS;
            if let Err(entry) = self.queues[dest].push(entry) {
                log::warn!("timeout queue overflow");
                entry.call();
            }
            self.sync_occupied(dest);
        }
        self.sync_occupied(window);
    }

    fn soft_advance(&mut self, now_ns: u64) -> Option<TimeoutEntry> {
        while self.soft_current < self.current {
            let window = self.soft_current % NR_WINDOWS;
            if let Some(t) = self.check_window(window, now_ns) {
                return Some(t);
            }
            self.rehome_undue(window);
            self.soft_current += 1;
        }
        let window = self.soft_current % NR_WINDOWS;
        if let Some(t) = self.check_window(window, now_ns) {
            return Some(t);
        }
        // Ceil placement puts a sub-tick deadline one window ahead of the head; checking it
        // early is safe because readiness is the ns deadline, not the visit.
        self.check_window((self.soft_current + 1) % NR_WINDOWS, now_ns)
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
    let (key, kick) = {
        let mut tq = TIMEOUT_QUEUE.lock();
        let (key, expire_ns) = tq.insert(time, timeout);
        // A deadline sooner than the bsp's programmed wake would otherwise wait out that wake
        // (up to a full tick) — the quantization that made every sub-ms sleep cost ~1ms.
        let kick = expire_ns != 0 && expire_ns.saturating_add(KICK_SLACK_NS) < tq.next_wake_abs_ns;
        (key, kick)
    };
    if kick {
        if current_processor().is_bsp() {
            check_reschedule_oneshot();
        } else {
            crate::processor::ipi::ipi_exec(
                crate::interrupt::Destination::Bsp,
                alloc::boxed::Box::new(check_reschedule_oneshot),
                false,
            );
        }
    }
    key
}

extern "C" fn soft_timeout_clock() {
    /* TODO: use some heuristic to decide if we need to spend more time handling timeouts */
    loop {
        let mut tq = TIMEOUT_QUEUE.lock();
        let timeout = tq.soft_advance(crate::instant::current_ns());
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

fn nano_to_ticks_ceil(time: Nanoseconds) -> u64 {
    time.div_ceil(NANOS_PER_TICK)
}

const NANOS_PER_TICK: Nanoseconds = 1_000_000;
/// Floor on any programmed oneshot, bounding the interrupt rate a burst of due-now deadlines
/// can produce while the timeout thread drains them. A chosen policy, not a hardware limit —
/// the APIC oneshot path expresses finer — so anything that someday wants sub-50µs timeouts
/// quantizes *here*, deliberately.
const MIN_ONESHOT_NS: Nanoseconds = 50_000;
/// A new deadline must beat the programmed wake by at least this much before we pay for a
/// reprogram (and possibly an IPI) to pull the wake in.
const KICK_SLACK_NS: Nanoseconds = 100_000;

/// Wheel advancement is by *measured* elapsed time on the bsp (sub-tick oneshots fire before a
/// whole tick passes; programmed-interval accounting would drift the wheel off real time).
/// The carry keeps the sub-tick remainder so no time is lost to the whole-tick division.
static BSP_LAST_NS: AtomicU64 = AtomicU64::new(0);
static BSP_CARRY_NS: AtomicU64 = AtomicU64::new(0);

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

/// Program the oneshot at ns resolution. `NEXT_TICK` still carries the whole-tick count for the
/// per-cpu tick stats (0 for a sub-tick wake); the arch layer takes nanoseconds directly.
fn schedule_oneshot_nanos(time: Nanoseconds) {
    NEXT_TICK.store(time / NANOS_PER_TICK, Ordering::SeqCst);
    crate::arch::schedule_oneshot_tick(time);
}

/// Pull the bsp's programmed wake in if the nearest timeout deadline is sooner — the "signal CPU
/// to wake up early" half of sub-tick timeouts. Non-bsp callers reach it by IPI (see
/// `register_timeout_callback`); timeouts are advanced only on the bsp.
pub fn check_reschedule_oneshot() {
    if !current_processor().is_bsp() {
        return;
    }
    crate::interrupt::with_disabled(|| {
        let now = crate::instant::current_ns();
        if now == 0 {
            return;
        }
        let mut timeout_queue = TIMEOUT_QUEUE.lock();
        let Some(delta) = timeout_queue.next_wake_delta_ns(now) else {
            return;
        };
        if delta == 0 {
            TIMEOUT_THREAD_CONDVAR.signal();
        }
        let programmed = delta.clamp(MIN_ONESHOT_NS, NANOS_PER_TICK);
        let deadline = now.saturating_add(programmed);
        if deadline.saturating_add(KICK_SLACK_NS) < timeout_queue.next_wake_abs_ns {
            timeout_queue.next_wake_abs_ns = deadline;
            schedule_oneshot_nanos(programmed);
        }
    });
}

pub fn oneshot_clock_hardtick() {
    let ticks = NEXT_TICK.load(Ordering::SeqCst);
    NR_CPU_TICKS.fetch_add(ticks, Ordering::SeqCst);
    let bsp_next_ns = if current_processor().is_bsp() {
        let now = crate::instant::current_ns();
        let whole = if now == 0 {
            ticks
        } else {
            let last = BSP_LAST_NS.swap(now, Ordering::Relaxed);
            if last == 0 {
                ticks
            } else {
                let elapsed = now.saturating_sub(last) + BSP_CARRY_NS.load(Ordering::Relaxed);
                BSP_CARRY_NS.store(elapsed % NANOS_PER_TICK, Ordering::Relaxed);
                elapsed / NANOS_PER_TICK
            }
        };
        BSP_TICK.fetch_add(whole, Ordering::SeqCst);
        let mut timeout_queue = TIMEOUT_QUEUE.lock();
        timeout_queue.hard_advance(whole as usize);
        let next = timeout_queue.next_wake_delta_ns(now);
        if next == Some(0) {
            // Due by ns in a head window `hard_advance`'s passed-window scan cannot see
            // (a sub-tick wake advances the wheel by zero whole ticks).
            TIMEOUT_THREAD_CONDVAR.signal();
        }
        // The scheduler pins the bsp to a one-tick cadence regardless (below), so the timer
        // gets the sooner of that and the nearest deadline.
        let programmed = next
            .unwrap_or(u64::MAX)
            .clamp(MIN_ONESHOT_NS, NANOS_PER_TICK);
        timeout_queue.next_wake_abs_ns = now.saturating_add(programmed);
        Some(programmed)
    } else {
        None
    };

    // Backstop for the requeue list. A waker can only park a thread there when it can't schedule
    // it directly (the target is critical, or hasn't set sync_sleep_done yet), and the waker's own
    // requeue_all() skips it for the same reason. The sleeper's claim_own_wakeup() covers the
    // common case, but draining every hardtick bounds how long anything can sit there if some
    // path doesn't -- without it, a single missed drain stalls that thread indefinitely.
    requeue_all();
    let sched_next_tick = schedule_hardtick();
    log::trace!(
        "hardtick {} {} {:?} {:?}",
        current_processor().id,
        ticks,
        sched_next_tick,
        bsp_next_ns
    );

    // Always rearm. The timer is one-shot, so a cpu that leaves a hardtick without programming the
    // next one takes no further timer interrupt for the rest of the boot -- it keeps running, and
    // keeps answering IPIs, so nothing looks wrong until you notice its timeslices never expire.
    //
    // The bsp is pinned to at most a one-tick cadence (its `bsp_next_ns` is clamped to
    // `NANOS_PER_TICK`, the old unconditional `Some(1)`), programmed at ns resolution so
    // sub-tick timeout deadlines actually fire on time. No other cpu advances timeouts:
    // a non-bsp cpu whose `schedule_hardtick` returns `None` -- any hardtick with no current
    // thread -- must still rearm or it retires its own clock permanently.
    if let Some(ns) = bsp_next_ns {
        schedule_oneshot_nanos(ns);
    } else {
        schedule_oneshot_tick(sched_next_tick.unwrap_or(REARM_TICKS));
    }
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
            // Slot 1 is the reserved best-realtime slot: a registered wall clock (kvmclock) when
            // there is one, otherwise the first-registered monotonic source as a fallback.
            clock_vec.push(ClockID(1));
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
            let idx: usize =
                id.0.try_into()
                    .map_err(|_| ArgumentError::InvalidArgument)?;
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
        crate::thread::entry::start_new_kernel(
            Priority::INTERRUPT,
            soft_timeout_clock,
            0,
            "soft-timeout-clock",
        )
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
