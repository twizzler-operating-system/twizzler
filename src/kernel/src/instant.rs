use core::{ops::Sub, time::Duration};

use twizzler_abi::syscall::{ClockFlags, ClockInfo, ClockSource, FemtoSeconds, TimeSpan};

use crate::{syscall::syscall_entry, time::TICK_SOURCES};

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Instant(TimeSpan);

impl Instant {
    pub fn now() -> Instant {
        let ticks = { TICK_SOURCES.lock()[0].read() };
        let span = ticks.value * ticks.rate;
        Instant(span)
    }

    #[allow(dead_code)]
    pub const fn zero() -> Instant {
        Instant(TimeSpan::ZERO)
    }

    #[allow(dead_code)]
    pub fn actually_monotonic() -> bool {
        use twizzler_runtime_api::Monotonicity;
        let runtime = twizzler_runtime_api::get_runtime();
        match runtime.actual_monotonicity() {
            Monotonicity::NonMonotonic => false,
            Monotonicity::Weak => true,
            Monotonicity::Strict => true,
        }
    }

    pub fn checked_sub_instant(&self, other: &Instant) -> Option<Duration> {
        Some(Duration::from(self.0.checked_sub(other.0)?))
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, rhs: Instant) -> Self::Output {
        self.checked_sub_instant(&rhs).unwrap_or(Duration::ZERO)
    }
}
