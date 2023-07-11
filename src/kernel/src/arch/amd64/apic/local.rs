use core::intrinsics::unlikely;

use crate::{arch::memory::phys_to_virt, clock::Nanoseconds, interrupt, memory::PhysAddr};

static mut LAPIC_ADDR: u64 = 0;

pub const LAPIC_ID: u32 = 0x20;
pub const LAPIC_VER: u32 = 0x30;
pub const LAPIC_TPR: u32 = 0x0080;
pub const LAPIC_EOI: u32 = 0x00b0;
pub const LAPIC_LDR: u32 = 0x00d0;
pub const LAPIC_DFR: u32 = 0x00e0;
pub const LAPIC_SVR: u32 = 0x00f0;
pub const LAPIC_ESR: u32 = 0x0280;
pub const LAPIC_ICRLO: u32 = 0x0300;
pub const LAPIC_ICRHI: u32 = 0x0310;
pub const LAPIC_TIMER: u32 = 0x0320;
pub const LAPIC_PCINT: u32 = 0x0340;
pub const LAPIC_LINT0: u32 = 0x0350;
pub const LAPIC_LINT1: u32 = 0x0360;
pub const LAPIC_ERROR: u32 = 0x0370;
pub const LAPIC_TICR: u32 = 0x0380;
pub const LAPIC_TDCR: u32 = 0x03e0;

pub const LAPIC_ICRLO_INIT: u32 = 0x0500;
pub const LAPIC_ICRLO_STARTUP: u32 = 0x0600;
pub const LAPIC_ICRLO_LEVEL: u32 = 0x8000;
pub const LAPIC_ICRLO_ASSERT: u32 = 0x4000;
pub const LAPIC_ICRLO_STATUS_PEND: u32 = 0x1000;

pub unsafe fn read_lapic(reg: u32) -> u32 {
    core::ptr::read_volatile((LAPIC_ADDR + reg as u64) as *const u32)
}

pub unsafe fn write_lapic(reg: u32, val: u32) {
    core::ptr::write_volatile((LAPIC_ADDR + reg as u64) as *mut u32, val);
    core::ptr::read_volatile((LAPIC_ADDR + LAPIC_ID as u64) as *const u32);
}

fn supports_deadline() -> bool {
    use crate::once::Once;
    static SUPPORTS_DEADLINE: Once<bool> = Once::new();
    *SUPPORTS_DEADLINE.call_once(|| {
        let cpuid = x86::cpuid::CpuId::new();
        let features = cpuid.get_feature_info().unwrap();
        features.has_tsc_deadline()
    })
}

static mut FREQ_MHZ: u64 = 0;
pub fn get_speeds() {
    let cpuid = x86::cpuid::CpuId::new();
    if !cpuid
        .get_advanced_power_mgmt_info()
        .unwrap()
        .has_invariant_tsc()
    {
        logln!("warning -- non-invariant TSC detected. Timing may be unpredictable.");
    }
    let tsc_speed_info = cpuid.get_tsc_info();
    if let Some(speed) = tsc_speed_info.map(|info| info.tsc_frequency()).flatten() {
        unsafe { FREQ_MHZ = speed / 1000000 };
        return;
    }
    if let Some(cpu_speed_info) = cpuid.get_processor_frequency_info() {
        unsafe { FREQ_MHZ = cpu_speed_info.processor_base_frequency() as u64 };
        if cpu_speed_info.processor_base_frequency() > 0 {
            return;
        }
    }
    if let Some(speed) = cpuid
        .get_hypervisor_info()
        .map(|info| info.tsc_frequency())
        .flatten()
    {
        unsafe { FREQ_MHZ = speed as u64 / 1000 };
        return;
    }
    unsafe { FREQ_MHZ = 2000 };
    logln!("warning -- failed to determine TSC frequency.");
}

pub fn init(bsp: bool) {
    if bsp {
        unsafe {
            let apic_base = x86::msr::rdmsr(x86::msr::APIC_BASE) as u32;
            LAPIC_ADDR =
                phys_to_virt(PhysAddr::new((apic_base & 0xffff0000) as u64).unwrap()).raw();
        }
        get_speeds();
    }

    unsafe {
        write_lapic(LAPIC_SVR, 0x100 | 0xff);

        write_lapic(LAPIC_TIMER, 0x10000 | 32);
        write_lapic(LAPIC_TICR, 10000000);
        write_lapic(LAPIC_LINT0, 0x10000);
        write_lapic(LAPIC_LINT1, 0x10000);

        write_lapic(LAPIC_ERROR, 0xfe);

        write_lapic(LAPIC_ESR, 0);
        write_lapic(LAPIC_ESR, 0);

        write_lapic(LAPIC_DFR, 0xffffffff);
        write_lapic(LAPIC_LDR, 1 << 24);

        write_lapic(LAPIC_EOI, 0);
        write_lapic(LAPIC_TPR, 0);
        write_lapic(LAPIC_TDCR, 0xb);
    }
}

pub fn eoi() {
    unsafe {
        write_lapic(LAPIC_EOI, 0);
    }
}

pub fn reset_error() {
    unsafe {
        write_lapic(LAPIC_ESR, 0);
    }
}

pub fn lapic_interrupt(irq: u16) {
    match irq {
        0xfe => panic!("LAPIC error"),
        0xf0 => crate::clock::oneshot_clock_hardtick(),
        0xf1 => crate::sched::schedule_resched(),
        _ => unimplemented!(),
    }
}

const LAPIC_TIMER_DEADLINE: u32 = 0x40000;
pub fn schedule_oneshot_tick(time: Nanoseconds) {
    let old = interrupt::disable();
    unsafe {
        let time = read_monotonic_nanoseconds() + time;
        let deadline = (time / 1000) * FREQ_MHZ;
        if supports_deadline() {
            write_lapic(LAPIC_TIMER, 240 | LAPIC_TIMER_DEADLINE);
            x86::msr::wrmsr(x86::msr::IA32_TSC_DEADLINE, deadline);
        } else {
            write_lapic(LAPIC_TIMER, 240);
            write_lapic(LAPIC_TICR, deadline as u32);
        }
    }
    interrupt::set(old);
}

pub fn read_monotonic_nanoseconds() -> Nanoseconds {
    // TODO: should we use rdtsc or rdtscp here? (the latter will require a cpuid check once (only
    // once, cache the result))
    let tsc = unsafe { x86::time::rdtsc() };
    let f = unsafe { FREQ_MHZ };
    if unlikely(f == 0) {
        panic!("cannot read nanoseconds before TSC calibration");
    }
    (tsc * 1000u64) / f
}
