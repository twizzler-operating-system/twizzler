/// Power State Coordination Interface (PSCI) is a standard interface for power management.
///
/// A full explanation of interfaces for power management can be found in the
/// "Arm Power State Coordination Interface Platform Design Document":
///     https://developer.arm.com/documentation/den0022/f/
use arm64::registers::Readable;
use arm64::registers::{MAIR_EL1, SCTLR_EL1, SPSR_EL1, TCR_EL1, TTBR0_EL1, TTBR1_EL1};
use smccc::psci::cpu_on;
use twizzler_abi::upcall::MemoryAccessKind;

use super::{translate, BootArgs};
use crate::{machine::info::devicetree, memory::VirtAddr, processor::Processor};

// According to Section 6.4 the MMU and caches are disabled
// and software must set the EL1h stack pointer
unsafe fn psci_secondary_entry(context_id: &BootArgs) -> ! {
    // TODO: manually set the configuration of registers

    // we need the lower half of memory identity mapped
    // this is because we are using physical addresses here
    // and when we turn on the mmu we still need to access
    // instructions and other data in lower memory
    core::arch::asm!(
        // set up the system registers needed by address translation
        "msr mair_el1, {}",
        "msr ttbr0_el1, {}",
        "msr ttbr1_el1, {}",
        "msr tcr_el1, {}",
        // ensure that all of these instructions commit
        "isb",
        // allow the use of FP instructions
        "msr cpacr_el1, {}",
        // set the entry point address (virtual)
        "msr elr_el1, {}",
        // set the stack pointer (virtual)
        // TODO: set this and then use aarch64 cpu stuff
        // TODO: verify if the way the stack grows is right
        "msr sp_el0, {}",
        // configure the execution state for EL1
        "msr spsr_el1, {}",
        // enable the MMU and caches
        "msr sctlr_el1, {}",
        // ensure that all other instructions commit
        // before executing other code with virtual
        // memory on
        "isb",
        // return to address specified in elr_el1
        "eret",
        in(reg) context_id.mair,
        in(reg) context_id.ttbr0,
        in(reg) context_id.ttbr1,
        in(reg) context_id.tcr,
        in(reg) context_id.cpacr,
        in(reg) context_id.entry,
        in(reg) context_id.kernel_stack,
        in(reg) context_id.spsr,
        in(reg) context_id.sctlr,
        options(noreturn, nostack),
    );
}

/// At this point we expect the MMU to be turned on
/// and paging to be functional. The executing environment
/// should be set up so we can execute safe Rust code.
fn rust_secondary_entry(args: &BootArgs) -> ! {
    // call the generic secondary cpu entry point
    crate::processor::secondary_entry(
        args.cpu,
        VirtAddr::new(args.tcb_base).unwrap(),
        args.kernel_stack as *mut u8,
    );
    // TODO: clean up values of registers saved after boot here
    // TODO: remove smp mappings, needs TLB coherence across cores
}

pub unsafe fn boot_core(core: &mut Processor, tcb_base: VirtAddr, kernel_stack: *mut u8) {
    // we will issue a CPU_ON to turn on the cpu core
    // first we will add the necessary arguments needed
    // by PSCI's CPU_ON function (Section 5.6)

    // pass cpu id, this is this core's MPIDR_EL1 value
    // TODO: ensure the right bits are 0
    let cpu_id = core.arch.mpidr;
    // pass secondary entry point (physical address)
    let entry_va = VirtAddr::new(psci_secondary_entry as u64).expect("invalid entry point address");
    let entry_pa = translate(entry_va, MemoryAccessKind::Read).expect("entry point is not mapped");
    // pass Context ID which in our implementation is the boot args
    // needed to start the CPU core. The Context ID is gaurenteed to
    // be passed as an argument to the entry point we specify.
    let context_id = &core.arch.args as *const _ as u64;
    let ctx_pa = translate(VirtAddr::new(context_id).unwrap(), MemoryAccessKind::Write)
        .expect("context ID is not mapped");

    // Here we pass in the necessary arguments to start the CPU

    let cpacr: u64;
    core::arch::asm!(
        "mrs {}, CPACR_EL1",
        out(reg) cpacr,
    );

    // Register state needed by low level code to setup an environment
    // suitable for executing Rust code in the kernel.
    core.arch.args.mair = MAIR_EL1.get();
    core.arch.args.ttbr1 = TTBR1_EL1.get();
    core.arch.args.ttbr0 = TTBR0_EL1.get();
    core.arch.args.tcr = TCR_EL1.get();
    core.arch.args.sctlr = SCTLR_EL1.get();
    core.arch.args.spsr = SPSR_EL1.get();
    core.arch.args.entry = rust_secondary_entry as u64;
    core.arch.args.cpacr = cpacr;

    // Things needed by the generic kernel code used to initialize this CPU core.
    core.arch.args.cpu = core.id;
    core.arch.args.tcb_base = tcb_base.raw();
    core.arch.args.kernel_stack = kernel_stack as u64;

    // get the method from the psci root node
    let method = {
        let psci_info = devicetree().find_node("/psci").expect("no psci node");
        psci_info
            .property("method")
            .expect("no method property")
            .as_str()
            .expect("failed to convert to string")
    };

    // here we assume 64 bit calling convention, in the future
    // we should check if this is different
    let boot_result = match method {
        "hvc" => cpu_on::<smccc::Hvc>(cpu_id, entry_pa.into(), ctx_pa.into()),
        _ => todo!("SMCCC calling convention needed by PSCI"),
    };
    // Booting up the core is asynchronous and the call only returns OK if the signal was sent
    if boot_result.is_err() {
        panic!("failed to start CPU core {}", core.id);
    }
}
