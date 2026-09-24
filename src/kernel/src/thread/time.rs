use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use crate::processor::{mp::MAX_CPU_ID, sched::CpuSet};

pub const SAMPLE_PERIOD_TICKS: u64 = 1;

#[derive(Debug, Default)]
pub struct ThreadStats {
    pub user: AtomicU64,
    pub sys: AtomicU64,
    pub idle: AtomicU64,
    pub last: AtomicU64,
    /// Page faults, pager page asks, and syscalls charged to this thread. Relaxed adds on paths
    /// that already do far more work than an increment, and read only as rates over a sample
    /// interval -- so a count landing either side of a preemption changes nothing.
    pub faults: AtomicU64,
    pub pager_pages: AtomicU64,
    pub syscalls: AtomicU64,
    /// Times this thread was made runnable from blocked. Charged in `schedule_thread`, which is
    /// the wake path proper -- not `schedule_thread_on_cpu`, which also carries preemption
    /// reinsertion and rebalance moves and would report those as wakes.
    pub wakes: AtomicU64,
    /// Switch-ins, and those onto a different cpu than the last one (`switch_to`).
    pub switches: AtomicU64,
    pub migrations: AtomicU64,
}

impl ThreadStats {
    /// Start accounting at `now` (statticks), so a thread's first sample doesn't charge it for
    /// all the time before it existed.
    pub fn new(now: u64) -> Self {
        Self {
            last: AtomicU64::new(now),
            ..Default::default()
        }
    }
}

/// The cpus a thread may run on. Read on every placement, so the words are atomics rather than
/// a lock; a torn read across words can only be a mask that existed at some point.
pub struct Affinity {
    words: [AtomicU64; MAX_CPU_ID / 64],
}

impl Affinity {
    pub const fn all() -> Self {
        Self {
            words: [const { AtomicU64::new(u64::MAX) }; MAX_CPU_ID / 64],
        }
    }

    pub fn allows(&self, cpu: u32) -> bool {
        let cpu = cpu as usize;
        cpu < MAX_CPU_ID && self.words[cpu / 64].load(Ordering::Relaxed) & (1 << (cpu % 64)) != 0
    }

    fn set(&self, set: &CpuSet) {
        for (word, value) in self.words.iter().zip(set.words()) {
            word.store(*value, Ordering::Relaxed);
        }
    }

    pub fn get(&self) -> CpuSet {
        let mut words = [0u64; MAX_CPU_ID / 64];
        for (out, word) in words.iter_mut().zip(self.words.iter()) {
            *out = word.load(Ordering::Relaxed);
        }
        CpuSet::from_words(words)
    }
}

impl core::fmt::Debug for Affinity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Affinity({} cpus)", self.get().count())
    }
}

#[derive(Debug)]
pub struct ThreadSched {
    pub last_cpu: AtomicI32,
    /// The single cpu the thread may run on, when [ThreadSched::affinity] names exactly one.
    /// Derived, never set on its own: it is the placement fast path and the run queues' notion
    /// of "movable".
    pub pinned_cpu: AtomicI32,
    /// The cpu that took the device interrupt waking this thread, for [select_cpu]'s irq-affine
    /// rule; -1 when the pending wake is not a device wake. Taken (cleared) by the next placement.
    ///
    /// [select_cpu]: crate::processor::sched
    irq_hint: AtomicI32,
    pub affinity: Affinity,
    /// Whether this thread was counted in its run queue's `movable` when inserted. Latched at
    /// insert and read at take, so an affinity change while queued cannot desync the count.
    queued_movable: AtomicBool,
    pub deadline: AtomicU64,
    /// `interact::now_ticks()` when this thread blocked, 0 while runnable; the wake credits the
    /// difference as voluntary sleep.
    pub sleep_tick: AtomicU64,
    pub current_processor_queue: AtomicI32,
    pub timeslice: AtomicU32,
    /// The hardtick expired this thread's slice; consumed at its reinsertion, which then files
    /// it at the calendar offset instead of the drain head a preempted thread gets.
    slice_end: AtomicBool,
    /// Queued by a wake and counted in its run queue's `pending_wakes`; cleared when taken.
    pub woken: AtomicBool,
    /// Bench-clock ns when this thread last took a cpu (`switch_to`); the wake granularity
    /// measures against it, not against paid ticks, so every cpu's tick cadence gives one answer.
    pub switched_in_ns: AtomicU64,
    /// When this thread was last made runnable, in raw bench-clock ticks, and how that wake was
    /// classified. Read and cleared when it next reaches a cpu, giving wake-to-run latency per
    /// wake rather than per boot -- which is the measurement the wake-latency work kept needing
    /// and inferring around. Zero means "no wake outstanding".
    ///
    /// Ticks, not nanoseconds. Converting one costs a u128 multiply and two u128 divisions (see
    /// [`crate::instant::Instant`]), and stamping in nanoseconds paid that at *both* ends of every
    /// wake -- on the wake path and again at the switch -- to measure an interval that needs one
    /// conversion in total.
    pub wake_ticks: AtomicU64,
    pub wake_kind: AtomicU32,
    /// Where the last insert filed this thread (`wakestats::queue_note`), under `--diag=wake`.
    pub queue_note: AtomicU64,
    /// Bench-clock stamp (`Instant::raw_ticks`) of when this thread last left a cpu
    /// (`switch_to`), for [ThreadSched::is_warm]. Not the scheduler tick: that advances in
    /// bursts when the bsp idles, so a 3-tick window read as 0 or as 10 and an 8 us ping-pong
    /// hop came out cold half the time.
    pub left_tick: AtomicU64,
}

/// How long after leaving a cpu a thread's cache footprint there is still worth chasing. ULE
/// uses 3 ms.
const WARM_NS: u64 = 3_000_000;
/// `WARM_NS` in bench-clock ticks, converted once so the wake path does no u128 division.
static WARM_RAW: AtomicU64 = AtomicU64::new(0);

fn warm_raw_ticks() -> Option<u64> {
    let raw = WARM_RAW.load(Ordering::Relaxed);
    if raw != 0 {
        return Some(raw);
    }
    let now = crate::instant::Instant::now();
    let ns_per_million = now.ns_since_ticks(now.raw_ticks().wrapping_sub(1_000_000));
    if ns_per_million == 0 {
        return None;
    }
    let raw = (WARM_NS as u128 * 1_000_000 / ns_per_million as u128).max(1) as u64;
    WARM_RAW.store(raw, Ordering::Relaxed);
    Some(raw)
}

impl Default for ThreadSched {
    fn default() -> Self {
        Self {
            last_cpu: AtomicI32::new(-1),
            pinned_cpu: AtomicI32::new(-1),
            irq_hint: AtomicI32::new(-1),
            affinity: Affinity::all(),
            queued_movable: AtomicBool::new(false),
            deadline: AtomicU64::new(0),
            sleep_tick: AtomicU64::new(0),
            current_processor_queue: AtomicI32::new(-1),
            timeslice: AtomicU32::new(0),
            slice_end: AtomicBool::new(false),
            woken: AtomicBool::new(false),
            switched_in_ns: AtomicU64::new(0),
            wake_ticks: AtomicU64::new(0),
            wake_kind: AtomicU32::new(0),
            queue_note: AtomicU64::new(0),
            left_tick: AtomicU64::new(0),
        }
    }
}

impl ThreadSched {
    /// Pin to one cpu without touching the affinity mask, for a kernel caller that needs to stay
    /// put briefly. [ThreadSched::set_affinity] re-derives the pin, so the two do not compose.
    pub fn pin_cpu(&self, cpu: u32) {
        self.pinned_cpu.store(cpu as i32, Ordering::Release);
    }

    pub fn set_irq_hint(&self, cpu: u32) {
        self.irq_hint.store(cpu as i32, Ordering::Release);
    }

    pub fn take_irq_hint(&self) -> Option<u32> {
        // Read first: most placements have no hint, and a swap would write this line every time.
        if self.irq_hint.load(Ordering::Acquire) < 0 {
            return None;
        }
        let cpu = self.irq_hint.swap(-1, Ordering::AcqRel);
        (cpu >= 0).then_some(cpu as u32)
    }

    pub fn unpin_cpu(&self) {
        self.pinned_cpu.store(-1, Ordering::Release);
    }

    /// Restrict the thread to `set`. Enforcement is the scheduler's: `select_cpu` only picks from
    /// the set, and a thread found running or queued outside it is re-placed at its next
    /// scheduling point.
    pub fn set_affinity(&self, set: &CpuSet) {
        self.affinity.set(set);
        match (set.count(), set.first()) {
            (1, Some(cpu)) => self.pin_cpu(cpu),
            _ => self.unpin_cpu(),
        }
    }

    /// Whether the thread left its last cpu recently enough that its data is likely still in
    /// that cpu's caches. A thread that never ran is cold.
    pub fn is_warm(&self) -> bool {
        let left = self.left_tick.load(Ordering::Relaxed);
        if left == 0 {
            return false;
        }
        let Some(window) = warm_raw_ticks() else {
            return true;
        };
        crate::instant::Instant::now()
            .raw_ticks()
            .wrapping_sub(left)
            <= window
    }

    pub fn note_queued_movable(&self) -> bool {
        let movable = self.pinned_to().is_none();
        self.queued_movable.store(movable, Ordering::Release);
        movable
    }

    pub fn queued_movable(&self) -> bool {
        self.queued_movable.load(Ordering::Acquire)
    }

    pub fn pinned_to(&self) -> Option<u32> {
        let cpu = self.pinned_cpu.load(Ordering::Acquire);
        if cpu >= 0 { Some(cpu as u32) } else { None }
    }

    pub fn pay_ticks(&self, ticks: u64, allowed: u64) -> bool {
        if self.timeslice.fetch_add(ticks as u32, Ordering::Acquire) as u64 + ticks >= allowed {
            self.timeslice.store(0, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn stamp_switch_in(&self, now_ns: u64) {
        self.switched_in_ns.store(now_ns, Ordering::Release);
    }

    /// Nanoseconds this thread has run since it last took a cpu.
    pub fn ran_ns(&self, now_ns: u64) -> u64 {
        now_ns.saturating_sub(self.switched_in_ns.load(Ordering::Acquire))
    }

    pub fn set_woken(&self) {
        self.woken.store(true, Ordering::Release);
    }

    pub fn take_woken(&self) -> bool {
        self.woken.swap(false, Ordering::AcqRel)
    }

    pub fn reset_timeslice(&self) {
        self.timeslice.store(0, Ordering::Release);
    }

    pub fn set_slice_end(&self) {
        self.slice_end.store(true, Ordering::Release);
    }

    pub fn take_slice_end(&self) -> bool {
        self.slice_end.swap(false, Ordering::AcqRel)
    }

    pub fn moving_to_queue(&self, cpu: u32) {
        self.current_processor_queue
            .store(cpu as i32, Ordering::Release);
    }

    pub fn moving_to_active(&self, cpu: u32) -> Option<u32> {
        self.current_processor_queue.store(-1, Ordering::Release);
        let old = self.last_cpu.swap(cpu as i32, Ordering::SeqCst);
        if old == -1 { None } else { Some(old as u32) }
    }

    pub fn current_cpu_rq(&self) -> Option<u32> {
        let cpu = self.current_processor_queue.load(Ordering::Acquire);
        if cpu >= 0 { Some(cpu as u32) } else { None }
    }

    /// Returns Some((cpu, pinned)), if either the thread is pinned or has a last cpu (in which case
    /// pinned = false).
    pub fn preferred_cpu(&self) -> Option<(u32, bool)> {
        let cpu = self.pinned_cpu.load(Ordering::Acquire);
        if cpu >= 0 {
            Some((cpu as u32, true))
        } else {
            let cpu = self.last_cpu.load(Ordering::Acquire);
            if cpu >= 0 {
                Some((cpu as u32, false))
            } else {
                None
            }
        }
    }

    pub fn set_deadline(&self, tick: u64) {
        self.deadline.store(tick, Ordering::Release);
    }

    pub fn get_deadline(&self) -> u64 {
        self.deadline.load(Ordering::Acquire)
    }
}
