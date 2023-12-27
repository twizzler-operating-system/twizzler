/// The `ClockHardware` interface for the CNTP_EL0 timer
/// This timer is local to a single core, and timestamps
/// are synchronized to a global system timer count

use arm64::registers::{CNTFRQ_EL0, CNTPCT_EL0, CNTP_CTL_EL0, CNTP_TVAL_EL0};
use registers::interfaces::{Readable, Writeable, ReadWriteable};

use crate::time::{ClockHardware, Ticks};

use twizzler_abi::syscall::{ClockFlags, ClockInfo, FemtoSeconds, TimeSpan, FEMTOS_PER_SEC};

/// The Non-secure physical timer `CNTP` for EL0.
pub struct PhysicalTimer {
    info: ClockInfo,
}

impl PhysicalTimer {
    /// According to "AArch64 Programmer's Guides Generic Timer"
    /// the physical timer has an interrupt ID of 30 usually
    pub const INTERRUPT_ID: u32 = 30;

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
                FemtoSeconds(FEMTOS_PER_SEC / freq),
                ClockFlags::MONOTONIC,
            ),
        }
    }

    // TODO: might need to make an API like this visible in ClockHardware

    /// set a timer to fire off an interrupt after some span of time
    pub fn set_timer(&self, span: TimeSpan) {
        // TODO: check more fined grained rates other than a second
        // should this fail if requested span is too low, or implicitly
        // round up

        // ticks = time / rate => span as femtos / rate (in femtos)
        let ticks =  span.as_femtos() / self.info.resolution().0 as u128;

        // configure the timer to fire after a certain amount of ticks have passed
        //
        // our division uses the u128 type, but the resulting value is truncated to
        // u64 since CNTP_TVAL_EL0 is also 64 bits.
        CNTP_TVAL_EL0.set(ticks as u64);

        // clear the interrupt mask and enable the timer
        CNTP_CTL_EL0.modify(
            CNTP_CTL_EL0::IMASK::CLEAR + CNTP_CTL_EL0::ENABLE::SET
        );
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

/// The interrupt handler for the aarch64 physical timer,
/// for now this does not do anything interesting. It merely
/// prints to the debug console and clears the interrupt.
pub fn cntp_interrupt_handler() {
    // emerglogln!("[arch:cntp] Hello from Timer!!");
    // handle the timer interrupt by advancing the scheduler ticks
    crate::clock::oneshot_clock_hardtick();

    // Disable the timer to clear the interrupt. Software must clear 
    // the interrupt before deactivating the interrupt in the
    // interrupt controller, otherwise it will keep firing.
    //
    // Alternatively we can mask the interrupt by setting
    // IMASK, or update the comparator.
    //
    // NOTE: disabling the timer does not stop the system
    // count from running, so reads from CNTPCT_EL0 are
    // still valid
    CNTP_CTL_EL0.modify(CNTP_CTL_EL0::ENABLE::CLEAR);
}
