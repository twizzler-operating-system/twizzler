use alloc::sync::Arc;

use no_std_moving_average::MovingAverage;
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

pub fn bench_clock() -> Option<Arc<dyn ClockHardware + Send + Sync>> {
    TICK_SOURCES.lock().get(0).cloned().flatten()
}

pub struct TimeStatCollector {
    running: MovingAverage<u64, u128, 10>,
    count: usize,
    mean: u128,
    running_mean: u128,
    variance: u128,
    min: u128,
    max: u128,
}

impl TimeStatCollector {
    pub fn new() -> Self {
        Self {
            running: MovingAverage::new(),
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
        self.running_mean = self.running.average(sample as u64) as u128;
        if sample < self.min {
            self.min = sample;
        }
        if sample > self.max {
            self.max = sample;
        }
        let old_total = self.mean * self.count as u128;
        let old_var = self.variance * self.count as u128;
        self.count += 1;
        self.mean = (old_total + sample) / self.count as u128;

        let delta = sample as i128 - self.mean as i128;
        self.variance = (old_var + (delta * delta) as u128) / self.count as u128;
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
