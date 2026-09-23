//! Cycle counters for [`crate::processor::freq`]: APERF/MPERF when the cpu exposes them, else the
//! fixed-function unhalted core and reference cycle counters, which hypervisors that hide
//! APERF/MPERF usually still virtualize. Both pairs advance at the core clock and the TSC's
//! nominal rate respectively, and both halt with the core.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use twizzler_abi::syscall::FreqSource;
use x86::msr::{
    IA32_APERF, IA32_FIXED_CTR_CTRL, IA32_FIXED_CTR1, IA32_FIXED_CTR2, IA32_MPERF,
    IA32_PERF_GLOBAL_CTRL, rdmsr, wrmsr,
};

use crate::processor::freq::CycleSample;

const MODE_NONE: u8 = 0;
const MODE_APERF_MPERF: u8 = 1;
const MODE_FIXED: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(MODE_NONE);
static FIXED_MASK: AtomicU64 = AtomicU64::new(u64::MAX);
static NOMINAL_KHZ: AtomicU64 = AtomicU64::new(0);

/// Decide the backend from CPUID, once, before any cpu samples. Must follow `Tsc::new`, which
/// establishes the reference rate.
pub fn init() {
    let cpuid = x86::cpuid::CpuId::new();
    let mode = if cpuid
        .get_thermal_power_info()
        .is_some_and(|t| t.has_hw_coord_feedback())
    {
        MODE_APERF_MPERF
    } else if let Some(pm) = cpuid
        .get_performance_monitoring_info()
        .filter(|pm| pm.version_id() >= 2 && pm.fixed_function_counters() >= 3)
    {
        let width = pm.fixed_function_counters_bit_width();
        FIXED_MASK.store(
            if width == 0 || width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            },
            Ordering::Relaxed,
        );
        MODE_FIXED
    } else {
        MODE_NONE
    };
    MODE.store(mode, Ordering::Release);

    // CPUID 0x16 is the rated base clock; the TSC runs at (approximately) that on anything
    // without the leaf, which is every hypervisor.
    let nominal = cpuid
        .get_processor_frequency_info()
        .map(|f| f.processor_base_frequency() as u64 * 1000)
        .filter(|khz| *khz != 0)
        .unwrap_or_else(|| super::tsc::nominal_hz() / 1000);
    NOMINAL_KHZ.store(nominal, Ordering::Relaxed);
    log::debug!("cpu frequency: {:?}, nominal {} kHz", source(), nominal);
}

pub fn init_this_cpu() {
    if MODE.load(Ordering::Acquire) != MODE_FIXED {
        return;
    }
    unsafe {
        // Fixed counter 1 (CPU_CLK_UNHALTED.CORE) and 2 (CPU_CLK_UNHALTED.REF_TSC), all rings.
        let ctrl = rdmsr(IA32_FIXED_CTR_CTRL) | (0b11 << 4) | (0b11 << 8);
        wrmsr(IA32_FIXED_CTR_CTRL, ctrl);
        let global = rdmsr(IA32_PERF_GLOBAL_CTRL) | (1 << 33) | (1 << 34);
        wrmsr(IA32_PERF_GLOBAL_CTRL, global);
    }
}

pub fn read_cycles() -> Option<CycleSample> {
    match MODE.load(Ordering::Acquire) {
        MODE_APERF_MPERF => unsafe {
            Some(CycleSample {
                actual: rdmsr(IA32_APERF),
                reference: rdmsr(IA32_MPERF),
            })
        },
        MODE_FIXED => unsafe {
            Some(CycleSample {
                actual: rdmsr(IA32_FIXED_CTR1),
                reference: rdmsr(IA32_FIXED_CTR2),
            })
        },
        _ => None,
    }
}

pub fn counter_mask() -> u64 {
    if MODE.load(Ordering::Acquire) == MODE_FIXED {
        FIXED_MASK.load(Ordering::Relaxed)
    } else {
        u64::MAX
    }
}

pub fn source() -> FreqSource {
    match MODE.load(Ordering::Acquire) {
        MODE_APERF_MPERF => FreqSource::AperfMperf,
        MODE_FIXED => FreqSource::FixedCounters,
        _ => FreqSource::Nominal,
    }
}

/// MPERF and the reference fixed counter both advance at the TSC's nominal rate.
pub fn reference_khz() -> u64 {
    super::tsc::nominal_hz() / 1000
}

pub fn nominal_khz() -> u64 {
    NOMINAL_KHZ.load(Ordering::Relaxed)
}
