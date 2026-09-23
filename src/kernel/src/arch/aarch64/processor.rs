use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering::SeqCst};

use arm64::{
    asm::{sev, wfe},
    registers::{CCSIDR_EL1, CLIDR_EL1, CSSELR_EL1, ID_AA64MMFR2_EL1, MPIDR_EL1, TPIDR_EL1},
};
use registers::interfaces::{Readable, Writeable};

use crate::{
    current_processor,
    machine::processor::{BootArgs, BootMethod},
    memory::VirtAddr,
    once::Once,
    processor::{
        Processor,
        sched::CPUTopoType,
        topology::{CacheDesc, CacheKind, TopoPath, TopoStep},
    },
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
    super::freq::init();
}

/// This cpu's path through the topology tree from its MPIDR affinity fields, and its caches
/// from CLIDR/CCSIDR. The registers do not say which cpus share a cache; LoUIS is the closest
/// thing: levels up to it are private to the core, and levels above it are taken as shared by
/// the cluster, which is how every ARM design so far is built.
pub fn get_topology() -> TopoPath {
    let mpidr = MPIDR_EL1.get();
    let aff = |n: u32| -> usize {
        let shift = if n == 3 { 32 } else { 8 * n };
        ((mpidr >> shift) & 0xff) as usize
    };
    // With MT set, Aff0 numbers the threads of a core and the core is one level up.
    let (package, cluster, core) = if mpidr & (1 << 24) != 0 {
        (aff(3), aff(2), aff(1))
    } else {
        (aff(2), aff(1), aff(0))
    };
    let steps = alloc::vec![
        TopoStep {
            index: package,
            kind: CPUTopoType::Package,
        },
        TopoStep {
            index: cluster,
            kind: CPUTopoType::Cluster,
        },
        TopoStep {
            index: core,
            kind: CPUTopoType::Core,
        },
    ];

    let clidr = CLIDR_EL1.get();
    let louis = ((clidr >> 21) & 7) as u8;
    let ccidx = ID_AA64MMFR2_EL1.read(ID_AA64MMFR2_EL1::CCIDX) != 0;
    let mut caches = Vec::new();
    for level in 1..=7u8 {
        // Ctype<n>: 0 no cache (and none above), 1 instruction, 2 data, 3 both, 4 unified.
        let kinds: &[(CacheKind, bool)] = match (clidr >> (3 * (level as u64 - 1))) & 7 {
            0 => break,
            1 => &[(CacheKind::Instruction, true)],
            2 => &[(CacheKind::Data, false)],
            3 => &[(CacheKind::Instruction, true), (CacheKind::Data, false)],
            _ => &[(CacheKind::Unified, false)],
        };
        let depth = if level <= louis { 3 } else { 2 };
        for (kind, instruction) in kinds {
            caches.push((depth, read_ccsidr(level, *instruction, ccidx, *kind)));
        }
    }
    TopoPath { steps, caches }
}

fn read_ccsidr(level: u8, instruction: bool, ccidx: bool, kind: CacheKind) -> CacheDesc {
    CSSELR_EL1
        .write(CSSELR_EL1::Level.val((level - 1) as u64) + CSSELR_EL1::InD.val(instruction as u64));
    unsafe { core::arch::asm!("isb", options(nostack, preserves_flags)) };
    let ccsidr = CCSIDR_EL1.get();
    let line_size = 1u32 << ((ccsidr & 7) as u32 + 4);
    let (ways, sets) = if ccidx {
        (
            ((ccsidr >> 3) & 0x1f_ffff) as u32 + 1,
            ((ccsidr >> 32) & 0xff_ffff) as u32 + 1,
        )
    } else {
        (
            ((ccsidr >> 3) & 0x3ff) as u32 + 1,
            ((ccsidr >> 13) & 0x7fff) as u32 + 1,
        )
    };
    CacheDesc {
        level,
        kind,
        size: ways as u64 * sets as u64 * line_size as u64,
        line_size,
        ways,
        sets,
        inclusive: false,
        fully_assoc: false,
    }
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
    pub fn wakeup(&self, signal: bool) {
        // remove the wait condition
        self.arch.wait_flag.store(false, SeqCst);
        // wakeup the processor
        sev();
        // `sev` only breaks an idle cpu out of `wfe`; a busy one needs the interrupt to notice
        // what was queued on it, as on x86.
        if signal {
            crate::interrupt::send_ipi(
                crate::interrupt::Destination::Single(self.id),
                super::interrupt::InterProcessorInterrupt::Reschedule,
            );
        }
    }
}

pub fn tls_ready() -> bool {
    TPIDR_EL1.get() != 0
}

/// This cpu's thread pointer: the kernel is built with `+tpidr-el1`, so that is the register its
/// TLS accesses go through. Only for computing a TLS offset once at startup.
pub fn tls_base() -> usize {
    TPIDR_EL1.get() as usize
}

pub fn spin_wait_iteration() {
    // tlb_shootdown_handler();
}
