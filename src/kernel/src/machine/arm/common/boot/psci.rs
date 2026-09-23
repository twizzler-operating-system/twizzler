/// Power State Coordination Interface (PSCI) is a standard interface for power management.
///
/// A full explanation of interfaces for power management can be found in the
/// "Arm Power State Coordination Interface Platform Design Document":
///     https://developer.arm.com/documentation/den0022/f/
use arm64::registers::Readable;
use arm64::registers::{MAIR_EL1, SCTLR_EL1, SPSR_EL1, TCR_EL1, TTBR0_EL1, TTBR1_EL1};
use smccc::psci::cpu_on;
use twizzler_abi::upcall::MemoryAccessKind;

use super::{BootArgs, translate};
use crate::{machine::info::devicetree, memory::VirtAddr, processor::Processor};

// According to Section 6.4 the MMU and caches are disabled
// and software must set the EL1h stack pointer
/// Entry point handed to PSCI. Entered with the MMU **off**, at this function's physical
/// address, with x0 = the physical address of the core's [`BootArgs`].
///
/// Naked deliberately: an ordinary Rust function's prologue writes to SP, and a freshly started
/// core's SP is undefined. It also cannot call anything, for the same reason.
#[unsafe(naked)]
unsafe extern "C" fn psci_secondary_entry(context_id: *const BootArgs) -> ! {
    core::arch::naked_asm!(
        "ldr x1, [x0, #{mair}]",
        "msr mair_el1, x1",
        "ldr x1, [x0, #{ttbr0}]",
        "msr ttbr0_el1, x1",
        "ldr x1, [x0, #{ttbr1}]",
        "msr ttbr1_el1, x1",
        "ldr x1, [x0, #{tcr}]",
        "msr tcr_el1, x1",
        "isb",

        "ldr x1, [x0, #{cpacr}]",
        "msr cpacr_el1, x1",
        "ldr x1, [x0, #{entry}]",
        "msr elr_el1, x1",
        // The stack is recorded by its base; the pointer starts at the top, as x86's trampoline
        // also does. Setting SP to the base put every push below the allocation.
        "ldr x1, [x0, #{stack}]",
        "mov x2, #{stack_size}",
        "add x1, x1, x2",
        "msr sp_el0, x1",
        "ldr x1, [x0, #{spsr}]",
        "msr spsr_el1, x1",
        // Read sctlr while x0 is still the *physical* args pointer: the MMU is off until the
        // store below, so a virtual x0 here would read a bogus address.
        "ldr x1, [x0, #{sctlr}]",
        // Hand the Rust entry the args by their kernel virtual address: the physical one we were
        // given is not mapped once the MMU is on.
        "ldr x0, [x0, #{self_va}]",
        "msr sctlr_el1, x1",
        "isb",
        "mov w10, #0x45",
        "eret",
        mair = const core::mem::offset_of!(BootArgs, mair),
        ttbr0 = const core::mem::offset_of!(BootArgs, ttbr0),
        ttbr1 = const core::mem::offset_of!(BootArgs, ttbr1),
        tcr = const core::mem::offset_of!(BootArgs, tcr),
        cpacr = const core::mem::offset_of!(BootArgs, cpacr),
        entry = const core::mem::offset_of!(BootArgs, entry),
        stack = const core::mem::offset_of!(BootArgs, kernel_stack),
        spsr = const core::mem::offset_of!(BootArgs, spsr),
        sctlr = const core::mem::offset_of!(BootArgs, sctlr),
        self_va = const core::mem::offset_of!(BootArgs, self_va),
        stack_size = const crate::processor::KERNEL_STACK_SIZE,
    )
}

fn rust_secondary_entry(args: &BootArgs) -> ! {
    // call the generic secondary cpu entry point
    crate::processor::mp::secondary_entry(
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
    // EL1t with DAIF masked, not the BSP's current SPSR_EL1. `psci_secondary_entry` puts the
    // secondary's stack in SP_EL0 and `init_secondary` is what promotes SPSel to ELx, so the
    // `eret` must land in EL1**t**. Inheriting worked only while the bsp itself ran on SP_EL0:
    // it now boots with SPSel=1, so the inherited value said EL1h and the secondary came up on
    // an uninitialized SP_EL1 -- before `exception::init`, so with no vectors to report it.
    core.arch.args.spsr = 0x3c4;
    core.arch.args.entry = rust_secondary_entry as u64;
    core.arch.args.cpacr = cpacr;

    // Things needed by the generic kernel code used to initialize this CPU core.
    core.arch.args.cpu = core.id;
    core.arch.args.tcb_base = tcb_base.raw();
    core.arch.args.kernel_stack = kernel_stack as u64;
    core.arch.args.self_va = &core.arch.args as *const BootArgs as u64;

    // The secondary reads these args with its MMU and caches OFF, so its loads bypass the
    // caches the BSP just wrote them through. Clean the struct to the point of coherency, or the
    // core comes up on stale ttbr/sctlr values and dies before it can install exception vectors.
    unsafe {
        let base = &core.arch.args as *const BootArgs as usize;
        let len = core::mem::size_of::<BootArgs>();
        // 64-byte lines cover every cortex-a class part this runs on; a smaller line just means
        // redundant cleans of the same line.
        let mut addr = base & !63;
        while addr < base + len {
            core::arch::asm!("dc cvac, {}", in(reg) addr);
            addr += 64;
        }
        core::arch::asm!("dsb sy");
    }

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
