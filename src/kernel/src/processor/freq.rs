//! The running frequency of each cpu, sampled from its cycle counters on its own statclock tick.
//!
//! Every backend (`arch::freq`) exposes two counters read together: one advancing at the core
//! clock and one at a fixed, known rate. Both stop while the core is halted, so their ratio over
//! a window is the frequency the core ran at while it ran -- idle time drops out, and no cpu has
//! to be interrupted to be asked.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub use twizzler_abi::syscall::FreqSource;

use crate::arch::freq as arch;

pub struct CycleSample {
    pub actual: u64,
    pub reference: u64,
}

pub struct FreqSampler {
    last_actual: AtomicU64,
    last_reference: AtomicU64,
    last_ns: AtomicU64,
    cur_khz: AtomicU64,
    sample_ns: AtomicU64,
    ready: AtomicBool,
}

/// Statticks land every ~8ms; one window is long enough that the number reads as a frequency
/// rather than as the ripple of every idle state the core passed through.
const MIN_WINDOW_NS: u64 = 100_000_000;

impl FreqSampler {
    pub const fn new() -> Self {
        Self {
            last_actual: AtomicU64::new(0),
            last_reference: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            cur_khz: AtomicU64::new(0),
            sample_ns: AtomicU64::new(0),
            ready: AtomicBool::new(false),
        }
    }

    /// From the statclock tick, on the cpu being sampled.
    pub fn tick(&self) {
        if !self.ready.load(Ordering::Relaxed) {
            arch::init_this_cpu();
            self.ready.store(true, Ordering::Relaxed);
        }
        let Some(sample) = arch::read_cycles() else {
            return;
        };
        let now = crate::instant::current_ns();
        let last_ns = self.last_ns.load(Ordering::Relaxed);
        if last_ns != 0 && now.saturating_sub(last_ns) < MIN_WINDOW_NS {
            return;
        }
        let last_actual = self.last_actual.swap(sample.actual, Ordering::Relaxed);
        let last_reference = self
            .last_reference
            .swap(sample.reference, Ordering::Relaxed);
        self.last_ns.store(now, Ordering::Relaxed);
        if last_ns == 0 {
            return;
        }
        let mask = arch::counter_mask();
        let actual = sample.actual.wrapping_sub(last_actual) & mask;
        let reference = sample.reference.wrapping_sub(last_reference) & mask;
        if reference == 0 {
            return;
        }
        let khz = (arch::reference_khz() as u128 * actual as u128 / reference as u128) as u64;
        self.cur_khz.store(khz, Ordering::Release);
        self.sample_ns.store(now, Ordering::Release);
    }

    /// The latest frequency in kHz, how it was obtained, and the monotonic time of the sample
    /// (0 when it is the nominal rate rather than a sample).
    pub fn current(&self) -> (u64, FreqSource, u64) {
        let sample_ns = self.sample_ns.load(Ordering::Acquire);
        if sample_ns != 0 {
            return (
                self.cur_khz.load(Ordering::Acquire),
                arch::source(),
                sample_ns,
            );
        }
        let nominal = arch::nominal_khz();
        let source = if nominal == 0 {
            FreqSource::Unknown
        } else {
            FreqSource::Nominal
        };
        (nominal, source, 0)
    }
}
