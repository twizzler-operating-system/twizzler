//! Implements the time runtime.

use core::time::Duration;

use twizzler_abi::syscall::{ClockSource, FastClock, ReadClockFlags};
use twizzler_rt_abi::time::Monotonicity;

use super::MinimalRuntime;

/// As in the reference runtime: one calibration syscall, then readings come from the CPU tick
/// counter. This runtime used to syscall on *every* reading of both clocks.
static MONOCLOCK: FastClock = FastClock::new(ClockSource::BestMonotonic, ReadClockFlags::empty());
static REALCLOCK: FastClock = FastClock::new(ClockSource::BestRealTime, ReadClockFlags::empty());

impl MinimalRuntime {
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
