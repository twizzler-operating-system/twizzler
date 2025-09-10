use alloc::sync::Arc;

use twizzler_abi::syscall::{ClockInfo, FemtoSeconds, FEMTOS_PER_NANO};

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
