//! Per-thread LLC-miss accounting behind the scheduler's cache-miss penalty (FreeBSD original:
//! `sched-cache.patch` at the repo root). Misses are charged from the cpu's free-running counter
//! at switch-out and on every stattick of the running thread, smoothed on that thread's own
//! statticks, and turned into a priority penalty that [`crate::thread::Thread::effective_priority`]
//! subtracts for User-class threads. All four knobs are constants to sweep over for now.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::Thread;
use crate::pmc;

/// Smoothed misses per stattick at or below which a thread pays nothing.
pub const THRESHOLD: u64 = 4_000;
/// Each further `STEP` misses per stattick costs one priority level.
pub const STEP: u64 = 2_000;
/// Three timeshare calendar buckets (`MAX_PRIORITY / NR_QUEUES` = 16 levels each). A User
/// thread's value saturates at 0, so the class never changes.
pub const MAX_PENALTY: u32 = 48;
/// Each stattick keeps `1 - 2^-DECAY_SHIFT` of the smoothed history.
pub const DECAY_SHIFT: u32 = 3;
/// Under a hypervisor a counter read is a VM exit (1.5-2 us on KVM, measured as +1.6-1.9 us per
/// context switch), so a switch-out read is skipped until this much cpu time has passed since
/// the cpu's last read; the skipped runs' misses land on the thread that finally reads, a lump
/// bounded to one such window. Bare metal reads at every switch: `rdpmc` is tens of cycles there.
pub const HV_CHARGE_MIN_NS: u64 = 1_000_000;

#[derive(Default, Debug)]
pub struct CacheMiss {
    /// Lifetime misses charged to this thread.
    total: AtomicU64,
    /// Charged since this thread's last stattick sample.
    pending: AtomicU64,
    /// Misses per stattick, scaled by `2^DECAY_SHIFT`.
    smoothed: AtomicU64,
    penalty: AtomicU32,
}

impl CacheMiss {
    fn charge(&self, misses: u64) {
        self.total.fetch_add(misses, Ordering::Relaxed);
        self.pending.fetch_add(misses, Ordering::Relaxed);
    }

    /// Fold the misses since the last sample into the rate and recompute the penalty. Called on
    /// this thread's stattick, so a sleeping thread keeps its penalty until it runs again.
    pub fn sample(&self) -> u32 {
        let pending = self.pending.swap(0, Ordering::Relaxed);
        let s = self.smoothed.load(Ordering::Relaxed);
        let s = s - (s >> DECAY_SHIFT) + pending;
        self.smoothed.store(s, Ordering::Relaxed);
        let penalty = penalty_for(s >> DECAY_SHIFT);
        self.penalty.store(penalty, Ordering::Relaxed);
        penalty
    }

    #[inline]
    pub fn penalty(&self) -> u32 {
        self.penalty.load(Ordering::Relaxed)
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    pub fn smoothed_per_tick(&self) -> u64 {
        self.smoothed.load(Ordering::Relaxed) >> DECAY_SHIFT
    }
}

pub const fn penalty_for(per_tick: u64) -> u32 {
    if per_tick <= THRESHOLD {
        0
    } else {
        let levels = (per_tick - THRESHOLD) / STEP;
        if levels > MAX_PENALTY as u64 {
            MAX_PENALTY
        } else {
            levels as u32
        }
    }
}

/// Minimum cpu time between switch-out reads: [`HV_CHARGE_MIN_NS`] under a hypervisor, else 0.
static CHARGE_MIN_NS: AtomicU64 = AtomicU64::new(0);

/// This cpu's counter at its last charge, and when that was.
#[thread_local]
static LAST_READ: AtomicU64 = AtomicU64::new(0);
#[thread_local]
static LAST_READ_NS: AtomicU64 = AtomicU64::new(0);

/// Once, on the bsp, after the counter is known to exist.
pub fn init() {
    if pmc::under_hypervisor() {
        CHARGE_MIN_NS.store(HV_CHARGE_MIN_NS, Ordering::Relaxed);
        logln!(
            "[kernel::cachemiss] hypervisor: switch-out reads at most every {} us",
            HV_CHARGE_MIN_NS / 1000
        );
    }
}

fn charge(thread: &Thread, now_ns: u64) {
    let now = pmc::read_local();
    let prev = LAST_READ.swap(now, Ordering::Relaxed);
    LAST_READ_NS.store(now_ns, Ordering::Relaxed);
    if !thread.is_idle_thread() {
        thread.cachemiss.charge(pmc::delta(prev, now));
    }
}

/// Charge this cpu's misses since its last read to `thread`, which is leaving the cpu. Switch
/// context: both reads must come from one cpu. Skipped while the cpu is inside its
/// [`CHARGE_MIN_NS`] window; the tick read catches up regardless.
pub fn charge_switch_out(thread: &Thread, now_ns: u64) {
    if !pmc::is_active() {
        return;
    }
    if now_ns.saturating_sub(LAST_READ_NS.load(Ordering::Relaxed))
        < CHARGE_MIN_NS.load(Ordering::Relaxed)
    {
        return;
    }
    charge(thread, now_ns);
}

/// The stattick's charge of the running thread: always reads.
pub fn charge_tick(thread: &Thread) {
    if !pmc::is_active() {
        return;
    }
    charge(thread, crate::instant::current_ns());
}

mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;
    use crate::thread::{entry::run_closure_in_new_thread, priority::Priority};

    #[kernel_test]
    fn test_penalty_for() {
        assert_eq!(penalty_for(0), 0);
        assert_eq!(penalty_for(THRESHOLD), 0);
        assert_eq!(penalty_for(THRESHOLD + STEP - 1), 0);
        assert_eq!(penalty_for(THRESHOLD + STEP), 1);
        assert_eq!(penalty_for(THRESHOLD + 3 * STEP), 3);
        assert_eq!(penalty_for(u64::MAX / 2), MAX_PENALTY);
    }

    #[kernel_test]
    fn test_sample_converges_to_rate() {
        let cm = CacheMiss::default();
        // (7/8)^256 is far below the integer floor, so this is the fixed point, not a tolerance.
        for _ in 0..256 {
            cm.charge(10 * STEP);
            cm.sample();
        }
        let per_tick = cm.smoothed_per_tick();
        assert!(
            per_tick >= 10 * STEP - 1 && per_tick <= 10 * STEP,
            "smoothed {} for a steady {}",
            per_tick,
            10 * STEP
        );
        assert_eq!(cm.penalty(), penalty_for(per_tick));
        for _ in 0..256 {
            cm.sample();
        }
        assert_eq!(cm.smoothed_per_tick(), 0);
        assert_eq!(cm.penalty(), 0);
    }

    /// A User thread doing a random walk over 32 MiB for several statticks must come out with a
    /// miss rate over the threshold and a penalty. The waiter blocks, so this holds at smp1.
    #[kernel_test]
    fn test_hostile_thread_is_penalized() {
        if !pmc::is_active() {
            logln!("[kernel::cachemiss] test skipped: no llc-miss counter");
            return;
        }
        let (thread, done) = run_closure_in_new_thread(Priority::USER, || {
            const WORDS: usize = 4 << 20;
            let mut buf = alloc::vec![0u64; WORDS];
            let mut idx = 1usize;
            let mut sum = 0u64;
            for _ in 0..(4 << 20) {
                idx = idx
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407)
                    & (WORDS - 1);
                sum = sum.wrapping_add(buf[idx]);
                buf[idx] = sum;
            }
            core::hint::black_box(&buf);
            sum
        });
        done.wait();
        let (total, per_tick, penalty) = (
            thread.cachemiss.total(),
            thread.cachemiss.smoothed_per_tick(),
            thread.cachemiss.penalty(),
        );
        logln!(
            "[kernel::cachemiss] hostile thread: {} misses, {} per tick, penalty {}",
            total,
            per_tick,
            penalty
        );
        assert!(total > 100_000, "only {} misses charged", total);
        assert!(penalty > 0, "no penalty at {} misses per tick", per_tick);
    }
}
