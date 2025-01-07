use core::str::FromStr;

use arm64::registers::MPIDR_EL1;
use registers::interfaces::Readable;

// re-export boot module
pub use crate::machine::arm::common::boot::*;
use crate::machine::info::devicetree;

pub fn enumerate_cpus() -> u32 {
    // MT bit means lowest level is logical cores (SMT)
    // U bit means we are running on a uniprocessor
    // combination of aff{3-0} is unique system wide

    // generally affinity 1 is the cluster ID, and
    // affinity 0 (bits [7:0]) is the core ID in the cluster
    let core_id = (MPIDR_EL1.get() & 0xff) as u32;

    // TODO: enumerate the cpus using a device tree
    crate::processor::register(core_id, core_id);
    let core = unsafe { crate::processor::get_processor_mut(core_id) };
    core.arch.mpidr = core_id as u64;

    core_id
}
