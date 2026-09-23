//! Cycle counters for [`crate::processor::freq`]: the Activity Monitors (FEAT_AMUv1) core-cycle
//! counter against the constant-cycle counter, which runs at CNTFRQ. Without an AMU there is no
//! counter that halts with the core, so nothing is sampled. The `aarch64-cpu` crate has none of
//! these registers, hence the raw encodings.

use core::{
    arch::asm,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use arm64::registers::CNTFRQ_EL0;
use registers::interfaces::Readable;
use twizzler_abi::syscall::FreqSource;

use crate::processor::freq::CycleSample;

static HAS_AMU: AtomicBool = AtomicBool::new(false);
static NOMINAL_KHZ: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    let pfr0: u64;
    unsafe {
        asm!("mrs {}, ID_AA64PFR0_EL1", out(reg) pfr0, options(nomem, nostack, preserves_flags));
    }
    let has_amu = (pfr0 >> 44) & 0xf != 0;
    HAS_AMU.store(has_amu, Ordering::Release);
    NOMINAL_KHZ.store(
        crate::machine::processor::nominal_hz().unwrap_or(0) / 1000,
        Ordering::Relaxed,
    );
    log::debug!(
        "cpu frequency: {:?}, nominal {} kHz",
        source(),
        nominal_khz()
    );
}

pub fn init_this_cpu() {
    if !HAS_AMU.load(Ordering::Acquire) {
        return;
    }
    // AMCNTENSET0_EL0: enable AMEVCNTR00 (CPU_CYCLES) and AMEVCNTR01 (CNT_CYCLES).
    unsafe {
        asm!("msr s3_3_c13_c2_1, {}", in(reg) 0b11u64, options(nomem, nostack, preserves_flags));
    }
}

pub fn read_cycles() -> Option<CycleSample> {
    if !HAS_AMU.load(Ordering::Acquire) {
        return None;
    }
    let (actual, reference): (u64, u64);
    unsafe {
        asm!(
            "mrs {}, s3_3_c13_c4_0",
            "mrs {}, s3_3_c13_c4_1",
            out(reg) actual,
            out(reg) reference,
            options(nomem, nostack, preserves_flags)
        );
    }
    Some(CycleSample { actual, reference })
}

pub fn counter_mask() -> u64 {
    u64::MAX
}

pub fn source() -> FreqSource {
    if HAS_AMU.load(Ordering::Acquire) {
        FreqSource::Amu
    } else if nominal_khz() != 0 {
        FreqSource::Nominal
    } else {
        FreqSource::Unknown
    }
}

pub fn reference_khz() -> u64 {
    CNTFRQ_EL0.get() / 1000
}

pub fn nominal_khz() -> u64 {
    NOMINAL_KHZ.load(Ordering::Relaxed)
}
