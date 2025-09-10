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
const APIC_ENABLE_X2MODE: u64 = 1 << 10;

// The APIC can either be a standard APIC or x2APIC. We'll support
// x2 eventually.
#[derive(PartialEq, Eq, Debug)]
enum ApicVersion {
    XApic,
    X2Apic,
}

#[derive(Debug)]
pub struct Lapic {
    version: ApicVersion,
    base: VirtAddr,
}

fn get_x2_msr_addr(reg: u32) -> u32 {
    // See Intel manual chapter of APIC -- all x2 registers are the original register values
    // shifted right 4 and offset by 0x800 into the MSR space.
    0x800 | (reg >> 4)
}

impl Lapic {
    fn new_apic(base: VirtAddr, version: ApicVersion) -> Self {
        Self { version, base }
    }

    /// Read a register from the local APIC.
    ///
    /// # Safety
    /// Caller must ensure that reg is a valid register in the APIC register space.
    pub unsafe fn read(&self, reg: u32) -> u32 {
        asm!("mfence;");
        match self.version {
            ApicVersion::XApic => {
                core::ptr::read_volatile(self.base.offset(reg as usize).unwrap().as_ptr())
            }
            ApicVersion::X2Apic => rdmsr(get_x2_msr_addr(reg)) as u32,
        }
    }

    /// Write a value to a register in the local APIC.
    ///
    /// # Safety
    /// Caller must ensure that reg is a valid register in the APIC register space.
    // Note: this does not need to take &mut self because the APIC is per-CPU.
    pub unsafe fn write(&self, reg: u32, val: u32) {
        asm!("mfence;");
        match self.version {
            ApicVersion::XApic => {
                core::ptr::write_volatile(
                    self.base.offset(reg as usize).unwrap().as_mut_ptr(),
                    val,
                );
                self.read(LAPIC_ID);
            }
            ApicVersion::X2Apic => wrmsr(get_x2_msr_addr(reg), val.into()),
        }
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

    /// Write the interrupt control register.
    pub fn write_icr(&self, hi: u32, lo: u32) {
        match self.version {
            ApicVersion::XApic => unsafe {
                const LAPIC_ICRHI_ID_OFFSET: u32 = 24;
                self.write(LAPIC_ICRHI, hi << LAPIC_ICRHI_ID_OFFSET);
                self.write(LAPIC_ICRLO, lo);
            },
            ApicVersion::X2Apic => {
                let val = ((hi as u64) << 32) | lo as u64;
                unsafe { wrmsr(get_x2_msr_addr(LAPIC_ICRLO), val) }
            }
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
            if matches!(self.version, ApicVersion::XApic) {
                self.write(LAPIC_DFR, !0);
            }
            self.write(LAPIC_TPR, 0);

            if matches!(self.version, ApicVersion::XApic) {
                // Assign all processors to group 1 in the logical addressing mode.
                self.write(LAPIC_LDR, 1 << 24);
            }

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

fn supports_x2_mode() -> bool {
    let cpuid = x86::cpuid::CpuId::new();
    let features = cpuid.get_feature_info().unwrap();
    features.has_x2apic()
}

fn global_enable() -> (PhysAddr, ApicVersion) {
    let mut base = unsafe { rdmsr(APIC_BASE) };
    base |= APIC_GLOBAL_ENABLE;
    if supports_x2_mode() {
        base |= APIC_ENABLE_X2MODE;
    }
    unsafe { wrmsr(APIC_BASE, base) };
    let vers = if supports_x2_mode() {
        ApicVersion::X2Apic
    } else {
        ApicVersion::XApic
    };
    (
        PhysAddr::new(base & !0xfff).expect("invalid APIC base address"),
        vers,
    )
}

static LAPIC: Once<Lapic> = Once::new();
/// Get a handle to the local APIC for this core. Note that the returned pointer
/// is actually shared by all cores, since there is no CPU-local state we need to keep
/// for now.
pub fn get_lapic() -> &'static Lapic {
    LAPIC.poll().expect("must initialize APIC before use")
}

/// Get a handle to the local APIC for this core. Note that the returned pointer
/// is actually shared by all cores, since there is no CPU-local state we need to keep
/// for now. If the LAPIC is not initialized, return None.
pub fn try_get_lapic() -> Option<&'static Lapic> {
    LAPIC.poll()
}

pub fn init(bsp: bool) {
    let apic = if bsp {
        let (base, version) = global_enable();
        logln!("[x86::apic] initializing APIC version {:?}", version);
        // TODO: make uncachable
        let apic = Lapic::new_apic(phys_to_virt(base), version);
        LAPIC.call_once(|| apic)
    } else {
        get_lapic()
    };
    if matches!(apic.version, ApicVersion::X2Apic) && !bsp {
        let mut base = unsafe { rdmsr(APIC_BASE) } | APIC_GLOBAL_ENABLE;
        if supports_x2_mode() {
            base |= APIC_ENABLE_X2MODE;
        }
        unsafe { wrmsr(APIC_BASE, base) };
    }
    apic.reset();
}

/// Handle an incoming internal APIC interrupt (or some IPIs).
pub fn lapic_interrupt(irq: u16) {
    match irq {
        LAPIC_ERR_VECTOR => panic!("LAPIC error"),
        LAPIC_TIMER_VECTOR => crate::clock::oneshot_clock_hardtick(),
        LAPIC_RESCHED_VECTOR => crate::processor::sched::schedule_resched(),
        _ => unimplemented!(),
    }
}
