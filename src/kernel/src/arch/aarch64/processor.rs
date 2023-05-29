use alloc::{vec::Vec};

use crate::{
    memory::VirtAddr,
    processor::Processor,
};

#[allow(unused_imports)] // DEBUG
use super::{interrupt::InterProcessorInterrupt};

pub fn init(_tls: VirtAddr) {
    todo!()
}

// register processors enumerated by hardware
// return the bootstrap processor id
pub fn enumerate_cpus() -> u32 {
    todo!()
}

/// Determine what hardware clock sources are available
/// on the processor and register them in the time subsystem.
pub fn enumerate_clocks() {
    todo!()
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
    // TODO: initlialize tls
    // see TPIDR_EL1
    false
}
