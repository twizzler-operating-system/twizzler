use core::{ops::Sub, time::Duration};

use twizzler_abi::syscall::TimeSpan;

use crate::time::{Ticks, TICK_SOURCES};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Instant(TimeSpan);

impl Instant {
    pub fn now() -> Instant {
        let ticks = {
            TICK_SOURCES
                .lock()
                .get(0)
                .map(|ts| ts.read())
                .unwrap_or(Ticks::default())
        };
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
            .map(|ts| ts.info().is_monotonic())
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
