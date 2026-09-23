//! The PL031 real-time clock: the wall clock on this machine.
//!
//! Read once at boot, then carried in generic-timer ticks so the clock shares a tick rate with the
//! monotonic source. Userspace's fast clock extrapolates every reading from `cntvct`, so a wall
//! clock reporting nanosecond ticks would run at the counter's rate instead of real time.

use arm64::registers::{CNTFRQ_EL0, CNTPCT_EL0};
use registers::interfaces::Readable;
use twizzler_abi::syscall::{ClockFlags, ClockInfo, FEMTOS_PER_SEC, FemtoSeconds, TimeSpan};

use crate::{
    memory::PhysAddr,
    time::{ClockHardware, Ticks},
};

struct Pl031 {
    /// Counter ticks at the Unix epoch, negative in wrapping arithmetic.
    base_ticks: u64,
    rate: FemtoSeconds,
}

impl ClockHardware for Pl031 {
    fn read(&self) -> Ticks {
        Ticks {
            value: CNTPCT_EL0.get().wrapping_add(self.base_ticks),
            rate: self.rate,
        }
    }

    fn info(&self) -> ClockInfo {
        ClockInfo::new(
            TimeSpan::ZERO,
            self.rate,
            self.rate,
            self.rate,
            ClockFlags::empty(),
        )
    }

    fn name(&self) -> &'static str {
        "pl031"
    }
}

pub fn register() {
    let Some(node) = super::info::devicetree().find_compatible(&["arm,pl031"]) else {
        return;
    };
    let Some(reg) = node.reg().and_then(|mut regs| regs.next()) else {
        return;
    };
    let va = super::super::common::mmio::map_device_region(
        unsafe { PhysAddr::new_unchecked(reg.starting_address as u64) },
        reg.size.unwrap_or(0x1000),
    );
    // RTCDR, at offset 0: seconds since the Unix epoch.
    let secs = unsafe { va.as_ptr::<u32>().read_volatile() } as u64;
    let freq = CNTFRQ_EL0.get();
    let base_ticks = secs.wrapping_mul(freq).wrapping_sub(CNTPCT_EL0.get());
    crate::time::register_best_realtime(Pl031 {
        base_ticks,
        rate: FemtoSeconds(FEMTOS_PER_SEC / freq),
    });
}
