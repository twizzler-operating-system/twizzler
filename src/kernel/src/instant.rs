use alloc::sync::Arc;
use core::{ops::Sub, time::Duration};

use twizzler_abi::syscall::TimeSpan;

use crate::{
    once::Once,
    time::{bench_clock, ClockHardware, Ticks, TICK_SOURCES},
};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Instant(TimeSpan);

static BENCH_CLOCK: Once<Arc<dyn ClockHardware + Send + Sync>> = Once::new();

fn get_bench() -> Option<&'static Arc<dyn ClockHardware + Send + Sync>> {
    if bench_clock().is_none() {
        return None;
    }
    Some(BENCH_CLOCK.call_once(|| bench_clock().unwrap()))
}

impl Instant {
    pub fn now() -> Instant {
        let ticks = { get_bench().map(|ts| ts.read()).unwrap_or(Ticks::default()) };
        let span = ticks.value * ticks.rate;
        Instant(span)
    }

    #[allow(dead_code)]
    pub const fn zero() -> Instant {
        Instant(TimeSpan::ZERO)
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
        Some(Duration::from(self.0.checked_sub(other.0)?))
    }

    pub fn into_time_span(self) -> TimeSpan {
        self.0
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, rhs: Instant) -> Self::Output {
        self.checked_sub_instant(&rhs).unwrap_or(Duration::ZERO)
    }
}
