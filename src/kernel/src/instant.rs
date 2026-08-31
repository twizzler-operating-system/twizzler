use alloc::sync::Arc;
use core::{ops::Sub, time::Duration};

use twizzler_abi::syscall::{FemtoSeconds, TimeSpan};

use crate::{
    once::Once,
    time::{ClockHardware, TICK_SOURCES, Ticks},
};

/// A timestamp, held as the raw tick count the clock reported plus the rate to interpret it with.
///
/// Deliberately *not* a [`TimeSpan`]. Converting ticks to one costs a u128 multiply and two u128
/// divisions (`Mul<FemtoSeconds> for u64`), and the overwhelmingly common use of an `Instant` is
/// to subtract it from another one -- so converting at `now()` pays for that twice per measured
/// interval, on paths that take a dozen readings. Kept as ticks, `now()` is an `rdtsc` and two
/// stores, and the conversion happens once, in the subtraction.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Instant {
    ticks: u64,
    rate: FemtoSeconds,
}

static BENCH_CLOCK: Once<Arc<dyn ClockHardware + Send + Sync>> = Once::new();

fn bench_clock() -> Option<Arc<dyn ClockHardware + Send + Sync>> {
    TICK_SOURCES.lock().get(0).cloned().flatten()
}

/// The `Once` exists to make this a one-time cost, but the guard in front of it was
/// `bench_clock().is_none()` -- which called `bench_clock()` *unconditionally*, so every
/// `Instant::now()` took the global `TICK_SOURCES` spinlock and cloned an `Arc` (a shared-cacheline
/// atomic RMW) before consulting the cache it already had. That is on the path of every
/// `Mutex::lock`, every page fault, and every scheduler tick, and it is a system-wide serialization
/// point rather than merely slow.
fn get_bench() -> Option<&'static Arc<dyn ClockHardware + Send + Sync>> {
    if let Some(clock) = BENCH_CLOCK.poll() {
        return Some(clock);
    }
    let clock = bench_clock()?;
    Some(BENCH_CLOCK.call_once(|| clock))
}

/// Monotonic nanoseconds from the bench clock, or 0 until one is registered. Cheap enough for
/// interrupt paths: one clock read plus u128 math (`Ticks::as_nanos` — the femtos-per-tick rate
/// of a fast clock is below 1e6, so the truncating u64 shortcut elsewhere would read 0 here).
pub fn current_ns() -> u64 {
    get_bench().map(|c| c.read().as_nanos() as u64).unwrap_or(0)
}

impl Instant {
    pub fn now() -> Instant {
        let ticks = { get_bench().map(|ts| ts.read()).unwrap_or(Ticks::default()) };
        Instant {
            ticks: ticks.value,
            rate: ticks.rate,
        }
    }

    #[allow(dead_code)]
    pub const fn zero() -> Instant {
        Instant {
            ticks: 0,
            rate: FemtoSeconds(0),
        }
    }

    #[allow(dead_code)]
    pub fn actually_monotonic() -> bool {
        TICK_SOURCES
            .lock()
            .get(0)
            .map(|ts| ts.as_ref().unwrap().info().is_monotonic())
            .unwrap_or_default()
    }

    pub fn checked_sub_instant(&self, other: &Instant) -> Option<Duration> {
        // `self`'s rate, not `other`'s: a reading taken from a clock is only meaningful against
        // that clock, and the tick source does not change after boot. An `Instant::zero()`
        // subtrahend carries no rate at all, which is exactly the case this must not consult.
        Some(Duration::from(
            self.ticks.checked_sub(other.ticks)? * self.rate,
        ))
    }

    pub fn into_time_span(self) -> TimeSpan {
        self.ticks * self.rate
    }

    /// The raw tick count, for stamping somewhere a whole `Instant` will not fit -- an
    /// `AtomicU64`, principally.
    ///
    /// Only meaningful against another reading of the same clock, which after boot is every
    /// reading: the tick source is registered once and never replaced. Pair with
    /// [`Instant::ns_since_ticks`] rather than converting the stamp itself, which is the whole
    /// point -- see the type's own doc comment for what a conversion costs.
    pub fn raw_ticks(&self) -> u64 {
        self.ticks
    }

    /// Nanoseconds from a stamp taken by [`Instant::raw_ticks`] to this reading, saturating at
    /// zero. One conversion for the interval, rather than one per endpoint.
    pub fn ns_since_ticks(&self, ticks: u64) -> u64 {
        (self.ticks.saturating_sub(ticks) * self.rate).as_nanos() as u64
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, rhs: Instant) -> Self::Output {
        self.checked_sub_instant(&rhs).unwrap_or(Duration::ZERO)
    }
}
