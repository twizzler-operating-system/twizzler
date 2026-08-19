//! Implements time routines.

use std::time::Duration;

use twizzler_abi::syscall::{ClockSource, FastClock, ReadClockFlags};
use twizzler_rt_abi::time::Monotonicity;

use super::ReferenceRuntime;

/// Both clocks read the CPU tick counter directly after a one-time calibration syscall, so
/// `Instant::now()` and `SystemTime::now()` do not enter the kernel. See [`FastClock`].
static MONOCLOCK: FastClock = FastClock::new(ClockSource::BestMonotonic, ReadClockFlags::empty());
static REALCLOCK: FastClock = FastClock::new(ClockSource::BestRealTime, ReadClockFlags::empty());

impl ReferenceRuntime {
    pub fn get_monotonic(&self) -> Duration {
        MONOCLOCK.get()
    }

    pub fn actual_monotonicity(&self) -> Monotonicity {
        Monotonicity::NonMonotonic
    }

    pub fn get_system_time(&self) -> Duration {
        REALCLOCK.get()
    }
}
