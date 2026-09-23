//! Intel architectural performance monitoring (SDM vol. 3, ch. 20): general-purpose counter 0
//! programmed for LONGEST_LAT_CACHE.MISS. AMD's core counters live at different MSRs and are not
//! handled yet; they report as unsupported.

use core::arch::x86_64::__cpuid;

use x86::msr::{IA32_PERF_GLOBAL_CTRL, IA32_PERFEVTSEL0, IA32_PMC0, rdmsr, wrmsr};

/// Event 0x2E, umask 0x41, counting user and kernel, enabled.
const LLC_MISS_EVSEL: u64 = 0x2e | (0x41 << 8) | (1 << 16) | (1 << 17) | (1 << 22);

pub struct Pmu {
    version: u32,
    pub width: u32,
}

/// Runs once, on the bsp. TCG guests report leaf 0xA as version 0, and KVM guests do too unless
/// qemu was given `pmu=on`; both must come back `Err` rather than touch an MSR.
pub fn probe() -> Result<Pmu, &'static str> {
    if __cpuid(0).eax < 0xa {
        return Err("no cpuid leaf 0xa");
    }
    let leaf = __cpuid(0xa);
    let version = leaf.eax & 0xff;
    if version == 0 {
        return Err("no architectural perfmon");
    }
    if (leaf.eax >> 8) & 0xff == 0 {
        return Err("no general-purpose counters");
    }
    // EBX bit 6 set means the LLC-miss event is *not* available; only meaningful when the
    // reported EBX vector is long enough to include it.
    if (leaf.eax >> 24) & 0xff > 6 && leaf.ebx & (1 << 6) != 0 {
        return Err("llc-miss event unavailable");
    }
    Ok(Pmu {
        version,
        width: (leaf.eax >> 16) & 0xff,
    })
}

/// Zero and start counter 0 on the calling cpu.
pub fn program_local(pmu: &Pmu) {
    unsafe {
        wrmsr(IA32_PERFEVTSEL0, 0);
        wrmsr(IA32_PMC0, 0);
        // Version 2 added the global enable; counters are gated on it regardless of EVSEL.EN.
        if pmu.version >= 2 {
            wrmsr(IA32_PERF_GLOBAL_CTRL, rdmsr(IA32_PERF_GLOBAL_CTRL) | 1);
        }
        wrmsr(IA32_PERFEVTSEL0, LLC_MISS_EVSEL);
    }
}

/// CPUID.1:ECX[31], set by every hypervisor that virtualizes the PMU.
pub fn under_hypervisor() -> bool {
    __cpuid(1).ecx & (1 << 31) != 0
}

/// Raw value of counter 0 on the calling cpu. Ring 0 needs no CR4.PCE.
#[inline]
pub fn read_local() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdpmc", in("ecx") 0u32, out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    ((hi as u64) << 32) | lo as u64
}
