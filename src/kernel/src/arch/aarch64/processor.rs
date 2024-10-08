use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering::SeqCst};

use arm64::{
    asm::{sev, wfe},
    registers::{MPIDR_EL1, TPIDR_EL1},
};
use registers::interfaces::{Readable, Writeable};

use crate::{
    current_processor,
    machine::processor::{BootArgs, BootMethod},
    memory::VirtAddr,
    once::Once,
    processor::Processor,
};

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
    // Get the local core number
    *BOOT_CORE_ID.call_once(|| {
        // enumerate all processors in a machine specific way
        crate::machine::processor::enumerate_cpus()
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
    // TODO: more sophisticated enumeration of CPUs
    // using something like information in MPIDR_EL1,
    // Device Tree, or ACPI

    // For now we simply return a the ID of this core.
    alloc::vec![((MPIDR_EL1.get() & 0xff) as usize, true)]
}

// arch specific implementation of processor specific state
#[derive(Default, Debug)]
pub struct ArchProcessor {
    pub boot: BootMethod,
    pub args: BootArgs,
    pub mpidr: u64,
    pub wait_flag: AtomicBool,
}

pub fn halt_and_wait() {
    /* TODO: spin a bit */
    /* TODO: actually put the cpu into deeper and deeper sleep, see PSCI */
    let core = current_processor();
    // set the wait condition
    core.arch.wait_flag.store(true, SeqCst);

    // wait until someone wakes us up
    while core.arch.wait_flag.load(SeqCst) {
        wfe();
    }
}

impl Processor {
    pub fn wakeup(&self, _signal: bool) {
        // remove the wait condition
        self.arch.wait_flag.store(false, SeqCst);
        // wakeup the processor
        sev();
    }
}

pub fn tls_ready() -> bool {
    TPIDR_EL1.get() != 0
}

pub fn spin_wait_iteration() {
    // tlb_shootdown_handler();
}
