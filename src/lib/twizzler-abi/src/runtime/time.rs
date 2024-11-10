//! Implements the time runtime.

use core::time::Duration;

use twizzler_rt_abi::{time::Monotonicity};

use super::MinimalRuntime;
use crate::syscall::{sys_read_clock_info, ClockSource, ReadClockFlags};

impl MinimalRuntime {
    pub fn get_monotonic(&self) -> Duration {
        let clock_info = sys_read_clock_info(ClockSource::BestMonotonic, ReadClockFlags::empty())
            .expect("failed to get monotonic time from kernel");
        Duration::from(clock_info.current_value())
    }

    pub fn actual_monotonicity(&self) -> Monotonicity {
        Monotonicity::NonMonotonic
    }

    pub fn get_system_time(&self) -> Duration {
        let clock_info = sys_read_clock_info(ClockSource::BestRealTime, ReadClockFlags::empty())
            .expect("failed to get monotonic time from kernel");
        Duration::from(clock_info.current_value())
    }
}
