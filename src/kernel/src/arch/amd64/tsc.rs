use crate::time::{ClockHardware, Ticks};

use twizzler_abi::syscall::{ClockInfo, FemtoSeconds};

pub struct TSC;

impl ClockHardware for TSC {
    fn read(&self) -> Ticks {
        Ticks{value:0,rate:FemtoSeconds(0)}
    }
    fn info(&self) -> ClockInfo {
        ClockInfo::ZERO
    }
}