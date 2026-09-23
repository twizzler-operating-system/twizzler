//! ARMv8 PMU: event counter 0 programmed for LL_CACHE_MISS_RD. Counting at EL0 and EL1.

/// PMCEID1_EL0 bit `event - 32` says whether the implementation counts it. QEMU's TCG PMU does
/// not, so `probe` fails there and the counter is never touched.
const LL_CACHE_MISS_RD: u64 = 0x37;

pub struct Pmu {
    pub width: u32,
}

pub fn probe() -> Result<Pmu, &'static str> {
    let pmcr: u64;
    let ceid1: u64;
    unsafe {
        core::arch::asm!("mrs {}, pmcr_el0", out(reg) pmcr, options(nomem, nostack));
        core::arch::asm!("mrs {}, pmceid1_el0", out(reg) ceid1, options(nomem, nostack));
    }
    if (pmcr >> 11) & 0x1f == 0 {
        return Err("no event counters");
    }
    if ceid1 & (1 << (LL_CACHE_MISS_RD - 32)) == 0 {
        return Err("LL_CACHE_MISS_RD not implemented");
    }
    // Event counters are 32 bits unless PMCR_EL0.LP is set, which this does not do.
    Ok(Pmu { width: 32 })
}

pub fn program_local(_pmu: &Pmu) {
    unsafe {
        core::arch::asm!(
            "msr pmevtyper0_el0, {ev}",
            "msr pmevcntr0_el0, xzr",
            "msr pmcntenset_el0, {one}",
            "mrs {t}, pmcr_el0",
            "orr {t}, {t}, #1",
            "msr pmcr_el0, {t}",
            "isb",
            ev = in(reg) LL_CACHE_MISS_RD,
            one = in(reg) 1u64,
            t = out(reg) _,
            options(nomem, nostack),
        );
    }
}

/// No cheap test exists here, and the only guests we run (TCG) have no counter to read anyway.
pub fn under_hypervisor() -> bool {
    false
}

#[inline]
pub fn read_local() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {}, pmevcntr0_el0", out(reg) v, options(nomem, nostack)) };
    v
}
