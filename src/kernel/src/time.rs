use alloc::sync::Arc;

use twizzler_abi::syscall::{ClockInfo, FEMTOS_PER_NANO, FemtoSeconds, TimeSpan, TimeStat};

use crate::{once::Once, spinlock::Spinlock};

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

pub const MAX_CLOCKS: usize = 8;
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

    // `break`, which this loop did not have: it filled *every* remaining slot with this one clock,
    // so the first registration consumed all `MAX_CLOCKS` of them, the clock list reported eight
    // copies of one clock, and any second `register_clock` found no free slot and was dropped on
    // the floor without a word.
    for pos in clock_list.iter_mut().skip(CLOCK_OFFSET) {
        if pos.is_none() {
            *pos = Some(clk.clone());
            break;
        }
    }
}

/// The registered tick sources, cached so that reading one is not a lock acquisition.
///
/// Sources are registered during boot and never replaced or removed, so a reading taken through
/// this cache can only be stale in the window before the source exists -- which
/// [`read_clock`] handles by falling back to the list itself.
static CLOCK_CACHE: [Once<Arc<dyn ClockHardware + Send + Sync>>; MAX_CLOCKS] =
    [const { Once::new() }; MAX_CLOCKS];

/// A/B: answer [`read_clock`] from the cache rather than by taking [`TICK_SOURCES`].
///
/// With this off, every `sys_read_clock_info` takes the one global tick-source spinlock, which is
/// what the syscall did before -- so every cpu in the system serialized against every other to ask
/// the time.
pub const CACHED_CLOCK_READ: bool = true;

/// Read tick source `idx`, without taking [`TICK_SOURCES`] once it has been read before.
pub fn read_clock(idx: usize) -> Option<Ticks> {
    if !CACHED_CLOCK_READ {
        return Some(TICK_SOURCES.lock().get(idx)?.as_ref()?.read());
    }
    let cache = CLOCK_CACHE.get(idx)?;
    if let Some(clock) = cache.poll() {
        return Some(clock.read());
    }
    let clock = TICK_SOURCES.lock().get(idx)?.clone()?;
    Some(cache.call_once(|| clock).read())
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
    /// Sum of samples, and sum of their squares. Mean and variance are derived from these in the
    /// getters; see [`TimeStatCollector::add_sample`].
    sum: u128,
    sum_sq: u128,
    min: u128,
    max: u128,
}

impl TimeStatCollector {
    pub fn new() -> Self {
        let running = MovingAverage::default();
        Self {
            running,
            count: 0,
            sum: 0,
            sum_sq: 0,
            min: u128::MAX,
            max: 0,
        }
    }

    pub fn add_sample(&mut self, time: TimeSpan) {
        let sample = time.as_femtos();
        if sample > u64::MAX as u128 {
            return;
        }
        self.running.add(sample as u64);
        if sample < self.min {
            self.min = sample;
        }
        if sample > self.max {
            self.max = sample;
        }
        // Sums, not running mean and variance. Deriving those on every sample cost two u128
        // divisions and two u128 multiplications per call -- and this is called once per syscall,
        // ~150,000 times a boot. The getters divide instead, and they run when someone asks for
        // `SysInfo`.
        //
        // Overflow is still dropped rather than wrapped, for the reason the previous version gave:
        // a wrapped accumulator poisons every value derived from it afterwards, while min/max and
        // the running average stay good.
        let Some(sum) = self.sum.checked_add(sample) else {
            return;
        };
        let Some(sq) = sample.checked_mul(sample) else {
            return;
        };
        let Some(sum_sq) = self.sum_sq.checked_add(sq) else {
            return;
        };
        self.sum = sum;
        self.sum_sq = sum_sq;
        self.count += 1;
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// Total of every sample, in femtoseconds. What a delta between two snapshots needs; `mean`
    /// times `count` is the same number only when neither has been rounded.
    pub fn sum_femtos(&self) -> u128 {
        self.sum
    }

    pub fn mean(&self) -> TimeSpan {
        TimeSpan::from_femtos(if self.count == 0 {
            0u128
        } else {
            self.sum / self.count as u128
        })
    }

    pub fn variance(&self) -> TimeSpan {
        if self.count == 0 {
            return TimeSpan::from_femtos(0u128);
        }
        let n = self.count as u128;
        let mean = self.sum / n;
        // E[x^2] - E[x]^2, saturating: with integer means the subtraction can go slightly negative
        // for a near-constant series.
        TimeSpan::from_femtos((self.sum_sq / n).saturating_sub(mean * mean))
    }

    pub fn min(&self) -> TimeSpan {
        TimeSpan::from_femtos(self.min)
    }

    pub fn max(&self) -> TimeSpan {
        TimeSpan::from_femtos(self.max)
    }

    pub fn running_mean(&self) -> TimeSpan {
        TimeSpan::from_femtos(self.running.get() as u128)
    }

    /// Fold another collector's samples into this one.
    ///
    /// Exact for everything except the running average, which is the caller's own and is left
    /// alone: summing the sample and squared-sample totals makes the merged mean and variance
    /// identical to what one collector fed every sample would report.
    pub fn merge(&mut self, other: &Self) {
        if other.count == 0 {
            return;
        }
        self.sum = self.sum.saturating_add(other.sum);
        self.sum_sq = self.sum_sq.saturating_add(other.sum_sq);
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
        self.count += other.count;
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
