use alloc::{vec::Vec};

use arm64::registers::{MPIDR_EL1, TPIDR_EL1};
use registers::interfaces::{Readable, Writeable};

use crate::{
    memory::VirtAddr,
    processor::Processor,
    once::Once,
};

#[allow(unused_imports)] // DEBUG
use super::{interrupt::InterProcessorInterrupt};

// initialize processor and any processor specific features
pub fn init(tls: VirtAddr) {
    // Save thread local storage to an unused variable.
    // We use TPIDR_EL1 for this purpose which is free
    // for the OS to use.
    TPIDR_EL1.set(tls.raw());
}

// the core ID of the bootstrap core
static BOOT_CORE_ID: Once<u32> = Once::new();

/// Register processors enumerated by hardware
/// and return the bootstrap processor's id
pub fn enumerate_cpus() -> u32 {
    // TODO: This is temporary until we can implement
    // enumeration of the CPUs using some specification
    // like Device Tree or ACPI

    // Get the local core number
    *BOOT_CORE_ID.call_once(|| {
        // generally affinity 1 is the cluster ID, and
        // affinity 0 (bits [7:0]) is the core ID in the cluster
        let core_id = (MPIDR_EL1.get() & 0xff) as u32;

        // For now we assume a single core, the boot core, and
        // return it's ID to the scheduling system
        crate::processor::register(core_id, core_id);
        core_id
    })
}

/// Determine what hardware clock sources are available
/// on the processor and register them in the time subsystem.
pub fn enumerate_clocks() {
    // for now we utlize the physical timer (CNTPCT_EL0)
    
    // save reference to the CNTP clock source into global array
    crate::time::register_clock(super::cntp::PhysicalTimer::new());
}

// map out topology of hardware
pub fn get_topology() -> Vec<(usize, bool)> {
    todo!()
}

// arch specific implementation of processor specific state
#[derive(Default, Debug)]
pub struct ArchProcessor;

pub fn halt_and_wait() {
    /* TODO: spin a bit */
    /* TODO: actually put the cpu into deeper and deeper sleep */
    todo!()
}

impl Processor {
    pub fn wakeup(&self, _signal: bool) {
        todo!()
    }
}

pub fn tls_ready() -> bool {
    TPIDR_EL1.get() != 0
}
