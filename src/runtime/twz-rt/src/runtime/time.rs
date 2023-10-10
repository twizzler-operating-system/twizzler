use std::time::Duration;

use twizzler_abi::syscall::{sys_read_clock_info, ClockSource, ReadClockFlags};
use twizzler_runtime_api::{Monotonicity, RustTimeRuntime};

use super::ReferenceRuntime;

// TODO: determine actual monotonicity properties

impl RustTimeRuntime for ReferenceRuntime {
    fn get_monotonic(&self) -> Duration {
        let clock_info = sys_read_clock_info(ClockSource::BestMonotonic, ReadClockFlags::empty())
            .expect("failed to get monotonic time from kernel");
        Duration::from(clock_info.current_value())
    }

    fn actual_monotonicity(&self) -> Monotonicity {
        Monotonicity::NonMonotonic
    }

    fn get_system_time(&self) -> Duration {
        let clock_info = sys_read_clock_info(ClockSource::BestRealTime, ReadClockFlags::empty())
            .expect("failed to get monotonic time from kernel");
        Duration::from(clock_info.current_value())
    }
}
