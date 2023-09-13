use core::time::Duration;

use twizzler_runtime_api::{RustInstant, RustTimeRuntime};

use crate::syscall::{sys_read_clock_info, ClockSource, ReadClockFlags};

use super::MinimalRuntime;

#[repr(transparent)]
pub struct TwzDuration(pub Duration);

impl RustTimeRuntime for MinimalRuntime {
    type InstantType = TwzDuration;

    type SystemTimeType = Duration;

    fn get_monotonic(&self) -> Self::InstantType {
        let clock_info = sys_read_clock_info(ClockSource::BestMonotonic, ReadClockFlags::empty())
            .expect("failed to get monotonic time from kernel");
        TwzDuration(Duration::from(clock_info.current_value()))
    }

    fn get_system_time(&self) -> Self::SystemTimeType {
        let clock_info = sys_read_clock_info(ClockSource::BestRealTime, ReadClockFlags::empty())
            .expect("failed to get monotonic time from kernel");
        Duration::from(clock_info.current_value())
    }
}

impl RustInstant for TwzDuration {
    fn actually_monotonic(&self) -> bool {
        false
    }
}

impl From<TwzDuration> for Duration {
    fn from(x: TwzDuration) -> Self {
        x.0
    }
}
