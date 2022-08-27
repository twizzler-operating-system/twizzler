//! Raw time handling, provides a way to get a monotonic timer and the system time. You should use
//! the rust standard library's time functions instead of these directly.

use core::time::Duration;

use crate::syscall::{sys_read_clock_info, ClockSource, ReadClockFlags};

/// Return a Duration representing an instant in monotonic time.
pub fn get_monotonic() -> Duration {
    let clock_info = sys_read_clock_info(ClockSource::BestMonotonic, ReadClockFlags::empty())
        .expect("failed to get monotonic time from kernel");
    Duration::from(clock_info.current_value())
}

/// Return a Duration representing the time since the unix epoch.
pub fn get_systemtime() -> Duration {
    let clock_info = sys_read_clock_info(ClockSource::BestRealTime, ReadClockFlags::empty())
        .expect("failed to get monotonic time from kernel");
    Duration::from(clock_info.current_value())
}
