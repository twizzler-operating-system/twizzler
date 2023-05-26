/// The `ClockHardware` interface for the CNTP_EL0 timer
/// This timer is local to a single core, and timestamps
/// are synchronized to a global system timer count

use arm64::registers::{CNTFRQ_EL0, CNTPCT_EL0};
use registers::interfaces::Readable;

use crate::time::{ClockHardware, Ticks};

use twizzler_abi::syscall::{ClockFlags, ClockInfo, FemtoSeconds, TimeSpan};

/// The Non-secure physical timer `CNTP` for EL0.
pub struct PhysicalTimer {
    info: ClockInfo,
}

impl PhysicalTimer {
    /// According to "AArch64 Programmer's Guides Generic Timer"
    /// the physical timer has an interrupt ID of 30 usually
    pub const INTERRUPT_ID: u64 = 30;

    pub fn new() -> Self {
        // The CNTFRQ_EL0 register holds the value of
        // the frequency of CNTP. The value is 32 bits
        // and in Hz.
        let freq = CNTFRQ_EL0.get();
        // logln!("[arch:timer] frequency: {} (Hz)", 1_000_000_000_000_000 / freq);
        Self {
            info: ClockInfo::new(
                TimeSpan::ZERO,
                FemtoSeconds(0), // TODO: precision
                FemtoSeconds(1_000_000_000_000_000 / freq),
                ClockFlags::MONOTONIC,
            ),
        }
    }
}

impl ClockHardware for PhysicalTimer {
    fn read(&self) -> Ticks {
        // The CNTPCT_EL0 register holds the current
        // count of CNTP. It the 64-bit physical timer count
        let count = CNTPCT_EL0.get();
        Ticks {
            value: count, // raw timer ticks (unitless)
            rate: self.info.resolution(),
        }
    }
    fn info(&self) -> ClockInfo {
        self.info
    }
}

pub fn cntp_interrupt_handler() {
    todo!("handle interrupts for physical timer")
}
