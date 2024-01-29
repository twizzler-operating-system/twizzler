/// The method of starting a CPU on ARM devices is machine specific
/// and usually implemented by the firmware.

mod psci;

use core::str::FromStr;

use arm64::registers::{PAR_EL1, Readable};

use twizzler_abi::upcall::MemoryAccessKind;

use crate::memory::{VirtAddr, PhysAddr};

/// Possible boot protocols used to start a CPU.
#[derive(Debug, Default, Copy, Clone, PartialEq)]
pub enum BootMethod {
    Psci,
    SpinTable,
    #[default]
    Unknown,
}

impl BootMethod {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Psci => "psci",
            Self::SpinTable => "spintable",
            Self::Unknown => "unknown",
        }
    }
}

impl FromStr for BootMethod {
    type Err = ();

    // Required method
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "psci" => Ok(BootMethod::Psci),
            "spin-table" => Ok(BootMethod::SpinTable),
            _ => Err(())
        }
    }
}

/// The arguments needed to start a CPU.
#[derive(Debug, Default, Copy, Clone)]
pub struct BootArgs {
    /// System-wide ID of this CPU core
    cpu: u32,
    /// TCB base use for TLS data
    tcb_base: u64,
    /// The stack of this kernel thread
    kernel_stack: u64,
    /// The entry point of this CPU core
    entry: u64,
    // system register state used to start core
    mair: u64,
    ttbr1: u64,
    ttbr0: u64,
    tcr: u64,
    sctlr: u64,
    spsr: u64,
    cpacr: u64,
}

/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(cpu: u32, tcb_base: VirtAddr, kernel_stack: *mut u8) {
    let core = unsafe {
        crate::processor::get_processor_mut(cpu)
    };
    logln!("starting {} with {}", core.id, core.arch.boot.as_str());

    match core.arch.boot {
        BootMethod::Psci => psci::boot_core(core, tcb_base, kernel_stack),
        _ => unimplemented!("boot method: {}", core.arch.boot.as_str())
    }
}

// Translate a virtual to a physical address if it is mapped in with the desired access rights
fn translate(va: VirtAddr, access: MemoryAccessKind) -> Option<PhysAddr> {
    if !va.is_kernel() {
        unimplemented!("address is in user memory: {:?}", va)
    }
    unsafe {
        // AT <operation>, <Xt>
        // <operation>: <stage><level><r/w>
        // - S1,E1,R/W (stage 1, EL1, R or Write)
        // <Xt>: address
        match access {
            MemoryAccessKind::Read => core::arch::asm!(
                "AT S1E1R, {}",
                in(reg) va.raw(),
                options(nostack, nomem),
            ),
            // given the way address translation works
            // writeable implies readable ...
            MemoryAccessKind::Write => core::arch::asm!(
                "AT S1E1W, {}",
                in(reg) va.raw(),
                options(nostack, nomem),
            ),
            _ => unimplemented!("translation for {:?}", access)
        }
    }
    // PAR_EL1 holds result of AT instruction
    // - FST: fault status info
    // - PA: output address
    logln!("{:?} -> {} {:#018x}", va, PAR_EL1.matches_all(PAR_EL1::F::TranslationSuccessfull), PAR_EL1.read(PAR_EL1::PA));
    if PAR_EL1.matches_all(PAR_EL1::F::TranslationSuccessfull) {
        let pa = unsafe { 
            // PAR_EL1.PA returns bits 47:12
            let base_phys = PAR_EL1.read(PAR_EL1::PA) << 12;
            // the lower 12 bit offset resides in the VA
            let block_offset = va.raw() & 0xFFF;
            PhysAddr::new_unchecked(base_phys | block_offset)
        };
        return Some(pa)
    }
    None
}
