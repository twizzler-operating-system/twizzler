//! ULE's dynamic timeshare priority for the User class: an interactivity score from voluntary
//! sleep versus run time, a decayed recent-cpu window, and the thread's own priority value as
//! its nice weight. The computed value is what `effective_priority` reports for a User thread.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::priority::MAX_PRIORITY;

/// Sleep/run history and the cpu window are kept in milliseconds shifted by this.
pub const SHIFT: u32 = 10;
/// Milliseconds per tick of the history clocks (`now_ticks`).
const HZ: u64 = 1000;
/// Statclock rate (`clock.rs` starts it at 127 Hz); a stattick of running charges this much.
const STAT_HZ: u64 = 127;
const TICKINCR: u64 = (HZ << SHIFT) / STAT_HZ;
/// Most sleep+run history kept before it is scaled back.
const SLP_RUN_MAX: u64 = (HZ * 5) << SHIFT;
/// Most history a new thread inherits from its spawner.
const SLP_RUN_FORK: u64 = (HZ / 2) << SHIFT;
pub const INTERACT_MAX: u32 = 100;
const INTERACT_HALF: u32 = INTERACT_MAX / 2;
/// Scores below this are interactive.
pub const INTERACT_THRESH: u32 = 30;
/// Cpu window: forget after `CPU_TGT`, rescale when the span exceeds `CPU_MAX` (decay 10/11 a
/// second).
const CPU_MAX: u64 = 11 * HZ;
const CPU_TGT: u64 = 10 * HZ;

/// User values `INTERACT_MIN..MAX_PRIORITY` are interactive, `0..=BATCH_MAX` batch. Batch
/// splits into `CPU_RANGE` levels of recent cpu use and `NICE_RANGE` of the base value.
pub const INTERACT_MIN: u16 = 84;
pub const BATCH_MAX: u16 = INTERACT_MIN - 1;
const NICE_RANGE: u16 = 40;
const CPU_RANGE: u16 = BATCH_MAX + 1 - NICE_RANGE;
const _: () = assert!(CPU_RANGE == 44 && INTERACT_MIN + 44 == MAX_PRIORITY);

pub fn now_ticks() -> u64 {
    crate::instant::current_ns() / 1_000_000
}

#[derive(Debug)]
pub struct Interact {
    runtime: AtomicU64,
    slptime: AtomicU64,
    /// Cpu window: running ticks (shifted) between `ftick` and `ltick`.
    ticks: AtomicU64,
    ftick: AtomicU64,
    ltick: AtomicU64,
    score: AtomicU32,
    /// The computed User value; the base value until the first `recompute`.
    value: AtomicU32,
    interactive: AtomicBool,
}

impl Interact {
    pub fn new(base_value: u16) -> Self {
        Self {
            runtime: AtomicU64::new(0),
            slptime: AtomicU64::new(0),
            ticks: AtomicU64::new(0),
            ftick: AtomicU64::new(0),
            ltick: AtomicU64::new(0),
            score: AtomicU32::new(0),
            value: AtomicU32::new(base_value as u32),
            interactive: AtomicBool::new(false),
        }
    }

    #[inline]
    pub fn value(&self) -> u16 {
        self.value.load(Ordering::Relaxed) as u16
    }

    #[inline]
    pub fn is_interactive(&self) -> bool {
        self.interactive.load(Ordering::Relaxed)
    }

    pub fn score(&self) -> u32 {
        self.score.load(Ordering::Relaxed)
    }

    /// Recent cpu use, percent of the window.
    pub fn cpu_pct(&self) -> u32 {
        let len = self
            .ltick
            .load(Ordering::Relaxed)
            .saturating_sub(self.ftick.load(Ordering::Relaxed))
            .max(1);
        ((self.ticks.load(Ordering::Relaxed) >> SHIFT) * 100 / len).min(100) as u32
    }

    /// One stattick of running.
    pub fn charge_run_tick(&self) {
        self.runtime.fetch_add(TICKINCR, Ordering::Relaxed);
        self.update();
    }

    /// `ticks` of voluntary sleep, credited at wake.
    pub fn credit_sleep(&self, ticks: u64) {
        if ticks == 0 {
            return;
        }
        self.slptime.fetch_add(ticks << SHIFT, Ordering::Relaxed);
        self.update();
    }

    /// Bound the history to `SLP_RUN_MAX`, keeping the sleep:run shape.
    fn update(&self) {
        let run = self.runtime.load(Ordering::Relaxed);
        let slp = self.slptime.load(Ordering::Relaxed);
        let sum = run + slp;
        if sum < SLP_RUN_MAX {
            return;
        }
        let (run, slp) = if sum > SLP_RUN_MAX * 2 {
            if run > slp {
                (SLP_RUN_MAX, 1)
            } else {
                (1, SLP_RUN_MAX)
            }
        } else if sum > (SLP_RUN_MAX / 5) * 6 {
            (run / 2, slp / 2)
        } else {
            ((run / 5) * 4, (slp / 5) * 4)
        };
        self.runtime.store(run, Ordering::Relaxed);
        self.slptime.store(slp, Ordering::Relaxed);
    }

    /// A new thread starts with its spawner's shape but at most `SLP_RUN_FORK` of it, so it is
    /// reclassified within half a second of doing something different.
    pub fn inherit_from(&self, parent: &Interact) {
        let mut run = parent.runtime.load(Ordering::Relaxed);
        let mut slp = parent.slptime.load(Ordering::Relaxed);
        let sum = run + slp;
        if sum > SLP_RUN_FORK {
            let ratio = sum / SLP_RUN_FORK;
            run /= ratio;
            slp /= ratio;
        }
        self.runtime.store(run, Ordering::Relaxed);
        self.slptime.store(slp, Ordering::Relaxed);
        self.ticks
            .store(parent.ticks.load(Ordering::Relaxed), Ordering::Relaxed);
        self.ftick
            .store(parent.ftick.load(Ordering::Relaxed), Ordering::Relaxed);
        self.ltick
            .store(parent.ltick.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    /// Decay the cpu window to `now`; `run` if the thread ran since the last update.
    pub fn pctcpu_update(&self, now: u64, run: bool) {
        let ltick = self.ltick.load(Ordering::Relaxed);
        let span = now.saturating_sub(ltick);
        if span >= CPU_TGT {
            self.ticks
                .store(if run { CPU_TGT << SHIFT } else { 0 }, Ordering::Relaxed);
            self.ftick
                .store(now.saturating_sub(CPU_TGT), Ordering::Relaxed);
            self.ltick.store(now, Ordering::Relaxed);
            return;
        }
        let ftick = self.ftick.load(Ordering::Relaxed);
        let mut ticks = self.ticks.load(Ordering::Relaxed);
        if now.saturating_sub(ftick) >= CPU_MAX {
            let len = ltick.saturating_sub(ftick).max(1);
            ticks = ticks / len * (CPU_TGT - span);
            self.ftick
                .store(now.saturating_sub(CPU_TGT), Ordering::Relaxed);
        }
        if run {
            ticks += span << SHIFT;
        }
        self.ticks.store(ticks, Ordering::Relaxed);
        self.ltick.store(now, Ordering::Relaxed);
    }

    fn interact_score(&self) -> u32 {
        let run = self.runtime.load(Ordering::Relaxed);
        let slp = self.slptime.load(Ordering::Relaxed);
        if run > slp {
            let div = (run / INTERACT_HALF as u64).max(1);
            return INTERACT_HALF + (INTERACT_HALF - (slp / div) as u32);
        }
        if slp > run {
            let div = (slp / INTERACT_HALF as u64).max(1);
            return (run / div) as u32;
        }
        if run != 0 { INTERACT_HALF } else { 0 }
    }

    /// Recompute the User value from the history, `base_value` as nice, and the cache-miss
    /// `penalty` (batch only). Returns the value.
    pub fn recompute(&self, base_value: u16, penalty: u32) -> u16 {
        let base = base_value.min(MAX_PRIORITY - 1) as i64;
        // Nice from the base value: 64 is 0, 127 is -20, 0 is +20; negative helps interactive.
        let nice = (64 - base) * 20 / 64;
        let score = (self.interact_score() as i64 + nice).max(0) as u32;
        self.score.store(score, Ordering::Relaxed);
        let value = if score < INTERACT_THRESH {
            self.interactive.store(true, Ordering::Relaxed);
            let range = (MAX_PRIORITY - INTERACT_MIN) as u32;
            (MAX_PRIORITY as u32 - 1 - range * score / INTERACT_THRESH) as u16
        } else {
            self.interactive.store(false, Ordering::Relaxed);
            let len = self
                .ltick
                .load(Ordering::Relaxed)
                .saturating_sub(self.ftick.load(Ordering::Relaxed))
                .max(1);
            let run = self.ticks.load(Ordering::Relaxed);
            let cpu_off = ((((CPU_RANGE - 1) as u64 * run + len / 2) / len + (1 << SHIFT) / 2)
                >> SHIFT)
                .min(CPU_RANGE as u64 - 1) as u16;
            let nice_off = ((127 - base) as u16 * NICE_RANGE + 63) / 127;
            (BATCH_MAX - cpu_off - nice_off).saturating_sub(penalty as u16)
        };
        self.value.store(value as u32, Ordering::Relaxed);
        value
    }
}

mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;

    #[kernel_test]
    fn test_fresh_thread_is_base_value() {
        let i = Interact::new(64);
        assert_eq!(i.value(), 64);
        assert!(!i.is_interactive());
    }

    #[kernel_test]
    fn test_spinner_is_batch_and_sleeper_interactive() {
        let spin = Interact::new(64);
        let mut now = 100_000;
        for _ in 0..500 {
            now += HZ / STAT_HZ;
            spin.pctcpu_update(now, true);
            spin.charge_run_tick();
        }
        let v = spin.recompute(64, 0);
        assert!(!spin.is_interactive(), "score {}", spin.score());
        assert!(v <= BATCH_MAX && v < 64, "spinner value {}", v);
        assert!(spin.cpu_pct() > 90, "cpu {}", spin.cpu_pct());

        let sleeper = Interact::new(64);
        sleeper.credit_sleep(4000);
        sleeper.charge_run_tick();
        let v = sleeper.recompute(64, 0);
        assert!(sleeper.is_interactive(), "score {}", sleeper.score());
        assert!(v >= INTERACT_MIN, "sleeper value {}", v);
        // Interactive threads never pay the cache-miss penalty.
        assert_eq!(sleeper.recompute(64, 40), v);
        // Batch threads do, and the base value orders batch threads with equal cpu use.
        let hi = Interact::new(127);
        let lo = Interact::new(0);
        assert!(hi.recompute(127, 0) > lo.recompute(0, 0));
        assert!(spin.recompute(64, 10) + 10 == spin.recompute(64, 0));
    }

    #[kernel_test]
    fn test_history_is_bounded() {
        let i = Interact::new(64);
        for _ in 0..100_000 {
            i.charge_run_tick();
        }
        assert!(
            i.runtime.load(Ordering::Relaxed) + i.slptime.load(Ordering::Relaxed)
                <= SLP_RUN_MAX * 2
        );
        let child = Interact::new(64);
        child.inherit_from(&i);
        assert!(
            child.runtime.load(Ordering::Relaxed) + child.slptime.load(Ordering::Relaxed)
                <= SLP_RUN_FORK * 2
        );
    }
}
