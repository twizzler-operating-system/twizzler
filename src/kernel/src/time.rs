use alloc::sync::Arc;

use twizzler_abi::syscall::{ClockInfo, FEMTOS_PER_NANO, FemtoSeconds, TimeSpan, TimeStat};

use crate::spinlock::Spinlock;

#[derive(Default, Debug, Clone, Copy)]
pub struct Ticks {
    pub value: u64,
    pub rate: FemtoSeconds,
}

impl Ticks {
    pub fn as_nanos(&self) -> u128 {
        (self.value as u128 * self.rate.0 as u128) / FEMTOS_PER_NANO as u128
    }
}

pub trait ClockHardware {
    fn read(&self) -> Ticks;
    fn info(&self) -> ClockInfo;
    fn name(&self) -> &'static str {
        ""
    }
}

const MAX_CLOCKS: usize = 8;
pub static TICK_SOURCES: Spinlock<[Option<Arc<dyn ClockHardware + Send + Sync>>; MAX_CLOCKS]> =
    Spinlock::new([const { None }; MAX_CLOCKS]);
pub const CLOCK_OFFSET: usize = 2;

pub fn register_clock<T>(clock: T)
where
    T: 'static + ClockHardware + Send + Sync,
{
    let clk = Arc::new(clock);
    let mut clock_list = TICK_SOURCES.lock();
    // this is a bit of a hack to reserve slots/id's 0 and 1
    // for the best monotonic and best real-time clocks
    // if not when we call sys_read_clock_info we'd have to
    // obtain a lock on USER_CLOCKS to get the clock id of the
    // best real-time or monotonic clock and then
    // TICK_SOURCES to read the data. References with Arc around
    // them still point to the same memory location.
    if clock_list[0].is_none() {
        clock_list[0] = Some(clk.clone());
    }
    if clock_list[1].is_none() {
        clock_list[1] = Some(clk.clone());
    }

    for pos in clock_list.iter_mut() {
        if pos.is_none() {
            *pos = Some(clk.clone());
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct MovingAverage {
    sum: u64,
    count: u64,
}

impl MovingAverage {
    // Adds a new value to the average safely
    fn add(&mut self, value: u64) {
        self.count += 1;
        let current_avg = self.get();
        if value >= current_avg {
            self.sum += (value - current_avg) / self.count;
        } else {
            self.sum -= (current_avg - value) / self.count;
        }
    }

    fn get(&self) -> u64 {
        if self.count == 0 { 0 } else { self.sum }
    }
}

pub struct TimeStatCollector {
    running: MovingAverage,
    count: usize,
    mean: u128,
    running_mean: u128,
    variance: u128,
    min: u128,
    max: u128,
}

impl TimeStatCollector {
    pub fn new() -> Self {
        let running = MovingAverage::default();
        Self {
            running,
            count: 0,
            mean: 0,
            variance: 0,
            min: u128::MAX,
            max: 0,
            running_mean: 0,
        }
    }

    pub fn add_sample(&mut self, time: TimeSpan) {
        let sample = time.as_femtos();
        if sample > u64::MAX as u128 {
            return;
        }
        self.running.add(sample as u64);
        self.running_mean = self.running.get() as u128;
        if sample < self.min {
            self.min = sample;
        }
        if sample > self.max {
            self.max = sample;
        }
        // Mean and variance are re-derived from `stat * count`, which does not fit u128 for large
        // samples: at the cap above, delta^2 alone is ~3.4e38 fs^2, and `variance * count` reaches
        // it far sooner. Drop the sample rather than wrap -- these are diagnostics, and a wrapped
        // accumulator poisons every value derived from it afterwards. min/max/running are
        // unaffected and stay updated.
        let count = self.count as u128;
        let Some(old_total) = self.mean.checked_mul(count) else {
            return;
        };
        let Some(old_var) = self.variance.checked_mul(count) else {
            return;
        };
        let Some(total) = old_total.checked_add(sample) else {
            return;
        };

        let new_count = count + 1;
        let new_mean = total / new_count;
        let delta = sample as i128 - new_mean as i128;
        let Some(delta_sq) = delta.checked_mul(delta) else {
            return;
        };
        let Some(var) = old_var.checked_add(delta_sq as u128) else {
            return;
        };

        self.count += 1;
        self.mean = new_mean;
        self.variance = var / new_count;
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn mean(&self) -> TimeSpan {
        TimeSpan::from_femtos(self.mean)
    }

    pub fn variance(&self) -> TimeSpan {
        TimeSpan::from_femtos(self.variance)
    }

    pub fn min(&self) -> TimeSpan {
        TimeSpan::from_femtos(self.min)
    }

    pub fn max(&self) -> TimeSpan {
        TimeSpan::from_femtos(self.max)
    }

    pub fn running_mean(&self) -> TimeSpan {
        TimeSpan::from_femtos(self.running_mean)
    }

    pub fn get_stats(&self) -> TimeStat {
        TimeStat {
            mean: self.mean(),
            running_mean: self.running_mean(),
            min: self.min(),
            max: self.max(),
            variance: self.variance(),
        }
    }
}
