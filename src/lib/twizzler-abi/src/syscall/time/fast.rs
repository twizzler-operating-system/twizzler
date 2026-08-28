//! A userspace clock that reads the CPU's tick counter directly, so that getting the time is not
//! a syscall.

use core::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use super::{sys_read_clock_info, ClockSource, ReadClockFlags};

/// Read the CPU's tick counter.
///
/// The same counter the kernel's registered tick source reads, which is what makes a cached rate
/// meaningful here: the two are the same clock, not two clocks that agree.
#[inline(always)]
fn tick_counter() -> u64 {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: rdtsc is unprivileged and has no side effects. It is not serializing, so a reading
    // may be reordered against surrounding work by a few tens of cycles; every caller here wants a
    // timestamp rather than a fence, and the serializing forms cost far more than they are worth.
    unsafe {
        core::arch::x86_64::_rdtsc()
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: CNTVCT_EL0 is readable at EL0 when CNTKCTL_EL1.EL0VCTEN is set, which the kernel
    // does for exactly this purpose.
    unsafe {
        let v: u64;
        core::arch::asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack));
        v
    }
}

/// How many bits of fraction [`FastClock`] keeps in its tick-to-nanosecond multiplier.
///
/// Picked per clock at calibration rather than fixed, because the multiplier has to fit in a u64:
/// a fast counter needs a big shift to stay precise, and a slow one would overflow at the same
/// shift. 48 is the ceiling because it is precise enough that truncation drift is unmeasurable
/// (~2^-48 relative, under a nanosecond per day) for every plausible tick rate.
const MAX_SHIFT: u32 = 48;

const FEMTOS_PER_NANO: u128 = 1_000_000;

const UNCALIBRATED: u64 = 0;
const CALIBRATING: u64 = 1;
const READY: u64 = 2;

/// A/B: read the tick counter in userspace at all. With this off every reading is a
/// `sys_read_clock_info` syscall -- which is what the static runtime did for both clocks, and what
/// the reference runtime did for the realtime clock, before this existed.
pub const FAST_USERSPACE_CLOCK: bool = true;

/// A/B: convert ticks with the precomputed multiply-and-shift rather than `TimeSpan::from_femtos`.
/// The latter is what the reference runtime's monotonic fast path used, and it costs two u128
/// divisions per reading. Only consulted when [`FAST_USERSPACE_CLOCK`] is on.
pub const FAST_CLOCK_MULSHIFT: bool = true;

/// A monotonic-ish clock that answers from the CPU's tick counter without entering the kernel.
///
/// One syscall, on the first reading, learns the tick rate and anchors the clock to whatever the
/// kernel says the time is right then. Every reading after that is a tick-counter read, a 64x64
/// multiply and a shift -- no syscall, no lock, and **no division**, which is the part that
/// matters: the obvious conversion (`TimeSpan::from_femtos`) costs two u128 divisions, and a u128
/// division is a libcall of some tens of cycles, so it dominated the reading it was converting.
///
/// State is published through atomics rather than a `OnceLock` so that this works in the `no_std`
/// runtime too. Exactly one thread calibrates, claimed by a compare-exchange on `ready`: letting
/// racing threads both store would let a reader pair *one* thread's `base_ticks` with the
/// *other*'s `base_ns`, and a mismatched anchor pair is a time that can step backwards. The
/// threads are microseconds apart so the step would be small, which is worse than large -- it
/// would violate monotonicity without ever being obvious.
pub struct FastClock {
    source: ClockSource,
    flags: ReadClockFlags,
    /// Nanoseconds the kernel reported at calibration.
    base_ns: AtomicU64,
    /// The tick counter read at calibration, subtracted from every later reading.
    base_ticks: AtomicU64,
    /// `ns = (ticks * mult) >> shift`.
    mult: AtomicU64,
    shift: AtomicU64,
    /// Only for the [`FAST_CLOCK_MULSHIFT`]-off arm.
    femtos_per_tick: AtomicU64,
    /// 0 = not calibrated, 1 = a thread is calibrating, 2 = the fields above are readable.
    /// Advanced to 2 last, with `Release`.
    ready: AtomicU64,
}

impl FastClock {
    pub const fn new(source: ClockSource, flags: ReadClockFlags) -> Self {
        Self {
            source,
            flags,
            base_ns: AtomicU64::new(0),
            base_ticks: AtomicU64::new(0),
            mult: AtomicU64::new(0),
            shift: AtomicU64::new(0),
            femtos_per_tick: AtomicU64::new(0),
            ready: AtomicU64::new(0),
        }
    }

    /// The current time, without entering the kernel once calibrated.
    pub fn get(&self) -> Duration {
        if !FAST_USERSPACE_CLOCK {
            return match sys_read_clock_info(self.source, self.flags) {
                Ok(info) => Duration::from(info.current_value()),
                Err(_) => Duration::ZERO,
            };
        }
        if self.ready.load(Ordering::Acquire) == READY {
            let ticks = tick_counter().wrapping_sub(self.base_ticks.load(Ordering::Relaxed));
            let base_ns = self.base_ns.load(Ordering::Relaxed);
            if !FAST_CLOCK_MULSHIFT {
                // The conversion this replaced: a u128 multiply and, inside `from_femtos`, a u128
                // division and a u128 modulo.
                let span = super::TimeSpan::from_femtos(
                    ticks as u128 * self.femtos_per_tick.load(Ordering::Relaxed) as u128,
                );
                return Duration::from_nanos(base_ns) + Duration::from(span);
            }
            let mult = self.mult.load(Ordering::Relaxed);
            let shift = self.shift.load(Ordering::Relaxed);
            let ns = base_ns + (((ticks as u128 * mult as u128) >> shift) as u64);
            return Duration::from_nanos(ns);
        }
        self.calibrate()
    }

    #[cold]
    fn calibrate(&self) -> Duration {
        let Ok(info) = sys_read_clock_info(self.source, self.flags) else {
            return Duration::ZERO;
        };
        // Whoever loses this answers from the syscall it just made and leaves the state alone.
        if self
            .ready
            .compare_exchange(
                UNCALIBRATED,
                CALIBRATING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return Duration::from(info.current_value());
        }
        let now = Duration::from(info.current_value());
        let femtos_per_tick = info.tickrate().0 as u128;
        // A clock that does not report its rate cannot be extrapolated from; fall back to asking
        // the kernel every time rather than inventing a rate.
        if femtos_per_tick == 0 {
            // Leave it uncalibrated rather than parked in CALIBRATING, so a clock that gains a
            // rate later can still be picked up.
            self.ready.store(UNCALIBRATED, Ordering::Release);
            return now;
        }

        // Largest shift whose multiplier still fits in a u64.
        let mut shift = MAX_SHIFT;
        let mult = loop {
            let m = (femtos_per_tick << shift) / FEMTOS_PER_NANO;
            if m <= u64::MAX as u128 {
                break m as u64;
            }
            // A rate slow enough to need this is a rate whose ticks are already coarse, so the
            // precision given up here is far below the clock's own resolution.
            shift -= 1;
        };

        let ticks = tick_counter();
        let base_ns = now.as_nanos() as u64;
        self.base_ticks.store(ticks, Ordering::Relaxed);
        self.base_ns.store(base_ns, Ordering::Relaxed);
        self.mult.store(mult, Ordering::Relaxed);
        self.femtos_per_tick
            .store(femtos_per_tick as u64, Ordering::Relaxed);
        self.shift.store(shift as u64, Ordering::Relaxed);
        // Last, and `Release`: a reader that sees this must see the stores above it.
        self.ready.store(READY, Ordering::Release);
        now
    }
}
