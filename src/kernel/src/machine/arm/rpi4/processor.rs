use core::str::FromStr;

use arm64::registers::MPIDR_EL1;
use registers::interfaces::Readable;

// use crate::machine::info::devicetree;

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

pub fn enumerate_cpus() -> u32 {
    // MT bit means lowest level is logical cores (SMT)
    // U bit means we are running on a uniprocessor
    // combination of aff{3-0} is unique system wide

    // generally affinity 1 is the cluster ID, and
    // affinity 0 (bits [7:0]) is the core ID in the cluster
    let core_id = (MPIDR_EL1.get() & 0xff) as u32;

    // explicitly enable the main core
    crate::processor::register(core_id, core_id);

    // TODO: parse the device tree
    // TODO: use spintables for processor boot

    // enumerate the cpus using a device tree
    // for cpu in devicetree().cpus() {
    //     emerglogln!("found cpu {}", cpu.ids().first());
    //     if core_id == cpu.ids().first() as u32 {
    //         // For now we assume a single core, the boot core, and
    //         // return it's ID to the scheduling system
    //         crate::processor::register(core_id, core_id);
    //         // set the enable method to turn on the CPU core
    //         if let Some(enable) = cpu.property("enable-method") {
    //             emerglogln!("\tenable = {:?}", enable.as_str());
    //             let core = unsafe {
    //                 crate::processor::get_processor_mut(core_id)
    //             };
    //             core.arch.boot = BootMethod::from_str(enable.as_str().unwrap()).unwrap();
    //         }
    //     }
    //     // TODO: register other processors so we can start them up
    // }

    core_id
}
