//! Implements time routines.

use std::{sync::OnceLock, time::Duration};

use twizzler_abi::syscall::{
    sys_read_clock_info, ClockInfo, ClockSource, ReadClockFlags, TimeSpan,
};
use twizzler_rt_abi::time::Monotonicity;

use super::ReferenceRuntime;

// TODO: determine actual monotonicity properties

struct MonoClock {
    source: ClockSource,
    flags: ReadClockFlags,
    info: OnceLock<ClockInfo>,
}

impl MonoClock {
    pub const fn new(source: ClockSource, flags: ReadClockFlags) -> Self {
        MonoClock {
            source,
            flags,
            info: OnceLock::new(),
        }
    }

    pub fn get_time(&self) -> Duration {
        if let Some(info) = self.info.get() {
            // We only set this if we get a non-zero tickrate.
            #[cfg(target_arch = "x86_64")]
            let cur = tick_counter::x86_64_tick_counter();
            #[cfg(target_arch = "aarch64")]
            let cur = tick_counter::aarch64_tick_counter();
            let time = TimeSpan::from_femtos(cur as u128 * info.tickrate().0 as u128);
            return Duration::from(time);
        }
        let clock_info = sys_read_clock_info(self.source, self.flags)
            .expect("failed to get monotonic time from kernel");
        if clock_info.tickrate().0 > 0 && self.info.get().is_none() {
            let _ = self.info.set(clock_info);
        }
        Duration::from(clock_info.current_value())
    }
}

static MONOCLOCK: MonoClock = MonoClock::new(ClockSource::BestMonotonic, ReadClockFlags::empty());

impl ReferenceRuntime {
    pub fn get_monotonic(&self) -> Duration {
        MONOCLOCK.get_time()
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
