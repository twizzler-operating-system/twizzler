use core::arch::asm;

use x86::msr::{rdmsr, wrmsr, APIC_BASE};

use crate::{
    arch::{amd64::tsc::Tsc, interrupt::TIMER_VECTOR, memory::phys_to_virt},
    clock::Nanoseconds,
    interrupt,
    memory::{PhysAddr, VirtAddr},
    once::Once,
    time::ClockHardware,
};

// Registers for local APIC
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

// Interrupt sending control flags
pub const LAPIC_ICRLO_INIT: u32 = 0x0500;
pub const LAPIC_ICRLO_STARTUP: u32 = 0x0600;
pub const LAPIC_ICRLO_LEVEL: u32 = 0x8000;
pub const LAPIC_ICRLO_ASSERT: u32 = 0x4000;
pub const LAPIC_ICRLO_STATUS_PEND: u32 = 0x1000;

// Timer flags
pub const LAPIC_TIMER_DEADLINE: u32 = 0x40000;

// Spurious vector register flags
pub const LAPIC_SVR_SOFT_ENABLE: u32 = 1 << 8;

pub const LAPIC_ERR_VECTOR: u16 = 0xfe;
pub const LAPIC_SPURIOUS_VECTOR: u16 = 0xff;
pub const LAPIC_TIMER_VECTOR: u16 = 0xf0;
pub const LAPIC_RESCHED_VECTOR: u16 = 0xf1;

// Timer divide register selections (3A 11.5.4)
pub const LAPIC_TDCR_DIV_1: u32 = 0xb;

// Shared flag across all interrupt control regs
pub const LAPIC_INT_MASKED: u32 = 1 << 16;

// Flags in the APIC base MSR
const APIC_BASE_BSP_FLAG: u64 = 1 << 8;
const APIC_GLOBAL_ENABLE: u64 = 1 << 11;

// The APIC can either be a standard APIC or x2APIC. We'll support
// x2 eventually.
#[derive(PartialEq, Eq)]
enum ApicVersion {
    XApic,
    X2Apic,
}

pub struct Lapic {
    version: ApicVersion,
    base: VirtAddr,
}

impl Lapic {
    fn new_apic(base: VirtAddr) -> Self {
        Self {
            version: ApicVersion::XApic,
            base,
        }
    }

    /// Read a register from the local APIC.
    ///
    /// # Safety
    /// Caller must ensure that reg is a valid register in the APIC register space.
    pub unsafe fn read(&self, reg: u32) -> u32 {
        core::ptr::read_volatile(self.base.offset(reg as usize).unwrap().as_ptr())
    }

    /// Write a value to a register in the local APIC.
    ///
    /// # Safety
    /// Caller must ensure that reg is a valid register in the APIC register space.
    // Note: this does not need to take &mut self because the APIC is per-CPU.
    pub unsafe fn write(&self, reg: u32, val: u32) {
        core::ptr::write_volatile(self.base.offset(reg as usize).unwrap().as_mut_ptr(), val);
        self.read(LAPIC_ID);
    }

    unsafe fn local_enable_set(&self, enable: bool) {
        if enable {
            self.write(
                LAPIC_SVR,
                LAPIC_SVR_SOFT_ENABLE | LAPIC_SPURIOUS_VECTOR as u32,
            )
        } else {
            self.write(LAPIC_SVR, LAPIC_SPURIOUS_VECTOR as u32)
        }
    }

    /// Issue an end-of-interrupt
    pub fn eoi(&self) {
        unsafe {
            self.write(LAPIC_EOI, 0);
        }
    }

    /// Clear the error status register.
    pub fn clear_err(&self) {
        unsafe {
            self.write(LAPIC_ESR, 0);
        }
    }

    fn reset(&self) {
        unsafe {
            self.local_enable_set(false);

            // Reset timer and basic interrupt control registers.
            self.write(LAPIC_TIMER, LAPIC_INT_MASKED | TIMER_VECTOR);
            self.write(LAPIC_TICR, 0);
            self.write(LAPIC_LINT0, LAPIC_INT_MASKED);
            self.write(LAPIC_LINT1, LAPIC_INT_MASKED);
            self.write(LAPIC_ERROR, LAPIC_ERR_VECTOR as u32);
            self.write(LAPIC_ESR, 0);
            self.write(LAPIC_DFR, !0);
            self.write(LAPIC_TPR, 0);

            // Assign all processors to group 1 in the logical addressing mode.
            self.write(LAPIC_LDR, 1 << 24);

            // Signal EOI
            self.write(LAPIC_EOI, 0);
            self.write(LAPIC_TDCR, LAPIC_TDCR_DIV_1);

            self.local_enable_set(true);
        }
    }

    /// Schedule the APIC timer to go off after `time` nanoseconds.
    pub fn setup_oneshot_timer(&self, time: Nanoseconds) {
        static TSC: Once<Tsc> = Once::new();
        let tsc = TSC.call_once(|| Tsc::new());
        let old = interrupt::disable();
        let tsc_val = tsc.read();
        let time_ticks = (1000000 * time) / tsc_val.rate.0;
        unsafe {
            // TODO: clean up once we do the cleanup for Nanoseconds.
            if supports_deadline() {
                let deadline = tsc_val.value + time_ticks;
                get_lapic().write(
                    LAPIC_TIMER,
                    LAPIC_TIMER_VECTOR as u32 | LAPIC_TIMER_DEADLINE,
                );
                // Intel 3A:11.5.4.1 requires an mfence here, between the MMIO write (if we're in
                // xAPIC mode) and the MSR write.
                if get_lapic().version == ApicVersion::XApic {
                    asm!("mfence;");
                }
                x86::msr::wrmsr(x86::msr::IA32_TSC_DEADLINE, deadline);
            } else {
                let apic = get_lapic();
                apic.write(LAPIC_TIMER, LAPIC_TIMER_VECTOR as u32);
                apic.write(LAPIC_TICR, time_ticks as u32);
            }
        }
        interrupt::set(old);
    }
}

fn supports_deadline() -> bool {
    static SUPPORTS_DEADLINE: Once<bool> = Once::new();
    *SUPPORTS_DEADLINE.call_once(|| {
        let cpuid = x86::cpuid::CpuId::new();
        let features = cpuid.get_feature_info().unwrap();
        features.has_tsc_deadline()
    })
}

fn global_enable() -> PhysAddr {
    let mut base = unsafe { rdmsr(APIC_BASE) };
    if base & APIC_GLOBAL_ENABLE == 0 {
        base |= APIC_GLOBAL_ENABLE;
        unsafe { wrmsr(APIC_BASE, base) };
    }
    PhysAddr::new(base & !0xfff).expect("invalid APIC base address")
}

static LAPIC: Once<Lapic> = Once::new();
/// Get a handle to the local APIC for this core. Note that the returned pointer
/// is actually shared by all cores, since there is no CPU-local state we need to keep
/// for now.
pub fn get_lapic() -> &'static Lapic {
    LAPIC.poll().expect("must initialize APIC before use")
}

pub fn init(bsp: bool) {
    let apic = if bsp {
        let base = global_enable();
        let apic = Lapic::new_apic(phys_to_virt(base));
        LAPIC.call_once(|| apic)
    } else {
        get_lapic()
    };
    apic.reset();
}

/// Handle an incoming internal APIC interrupt (or some IPIs).
pub fn lapic_interrupt(irq: u16) {
    match irq {
        LAPIC_ERR_VECTOR => panic!("LAPIC error"),
        LAPIC_TIMER_VECTOR => crate::clock::oneshot_clock_hardtick(),
        LAPIC_RESCHED_VECTOR => crate::sched::schedule_resched(),
        _ => unimplemented!(),
    }
}
