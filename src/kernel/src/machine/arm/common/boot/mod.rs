/// The method of starting a CPU on ARM devices is machine specific
/// and usually implemented by the firmware.

use core::str::FromStr;

use crate::memory::VirtAddr;

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

/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(_cpu: u32, _tcb_base: VirtAddr, _kernel_stack: *mut u8) {
    todo!("start a core") 
}