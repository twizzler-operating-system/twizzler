use alloc::{sync::Arc, vec::Vec};

use twizzler_abi::syscall::{ClockInfo, FemtoSeconds};

use crate::spinlock::Spinlock;

#[derive(Default, Debug, Clone, Copy)]
pub struct Ticks {
    pub value: u64,
    pub rate: FemtoSeconds,
}

pub trait ClockHardware {
    fn read(&self) -> Ticks;
    fn info(&self) -> ClockInfo;
}

pub static TICK_SOURCES: Spinlock<Vec<Arc<dyn ClockHardware + Send + Sync>>> =
    Spinlock::new(Vec::new());
pub const CLOCK_OFFSET: usize = 2;

pub fn register_clock<T>(clock: T)
where
    T: 'static + ClockHardware + Send + Sync,
{
    let mut clock_list = TICK_SOURCES.lock();
    let clk_id = clock_list.len();
    let clk = Arc::new(clock);
    clock_list.push(clk.clone());
    // this is a bit of a hack to reserve slots/id's 0 and 1
    // for the best monotonic and best real-time clocks
    // if not when we call sys_read_clock_info we'd have to
    // obtain a lock on USER_CLOCKS to get the clock id of the
    // best real-time or monotonic clock and then
    // TICK_SOURCES to read the data. References with Arc around
    // them still point to the same memory location.
    #[allow(unused_unsafe)]
    if unsafe { core::intrinsics::unlikely(clk_id == 0) } {
        // reserve space for the real-time clock
        clock_list.push(clk.clone());
        // offset location of this clock source
        clock_list.push(clk.clone());
    }
}
