use arm64::registers::{SPSel, TPIDR_EL1};
use registers::{
    interfaces::{Readable, Writeable},
    registers::InMemoryRegister,
};
use twizzler_abi::syscall::TimeSpan;

use crate::{BootInfo, clock::Nanoseconds, syscall::SyscallContext};

pub mod address;
mod cntp;
pub mod context;
pub mod freq;
mod exception;
pub mod image;
pub mod interrupt;
pub mod memory;
pub mod pmc;
pub mod processor;
mod start;
mod syscall;
pub mod thread;

pub use address::{PhysAddr, VirtAddr};
pub use interrupt::{init_interrupts, send_ipi, set_interrupt};
pub use start::BootInfoSystemTable;

/// Let EL0 read CNTPCT/CNTVCT (and so CNTFRQ) directly: userspace clocks read the counter.
fn enable_el0_counters() {
    unsafe { core::arch::asm!("msr cntkctl_el1, {}", in(reg) 0b11u64) };
}

pub fn init() {
    // initialize exceptions by setting up our exception vectors
    exception::init();
    enable_el0_counters();
    // configure registers needed by the memory management system
    // TODO: configure MAIR

    // On reset, TPIDR_EL1 is initialized to some unknown value.
    // we set it to zero so that we know it is not initialized.
    TPIDR_EL1.set(0);
}

pub fn init_post_memory<B: BootInfo + Send + Sync + 'static + ?Sized>(boot_info: &B) {
    // Initialize the machine specific enumeration state (e.g., DeviceTree, ACPI)
    crate::machine::info::init(boot_info);
}

pub fn init_secondary() {
    // initialize exceptions by setting up our exception vectors
    exception::init();
    enable_el0_counters();

    // check if SPSel is already set to use SP_EL1
    let spsel: InMemoryRegister<u64, SPSel::Register> = InMemoryRegister::new(SPSel.get());
    if spsel.matches_all(SPSel::SP::EL0) {
        // make it so that we use SP_EL1 in the kernel
        // when taking an exception.
        spsel.write(SPSel::SP::ELx);
        let sp: u64;
        unsafe {
            core::arch::asm!(
                // save the stack pointer from before
                "mov {0}, sp",
                // change usage of sp from SP_EL0 to SP_EL1
                "msr spsel, {1}",
                // set current stack pointer to previous,
                // sp is now aliased to SP_EL1
                "mov sp, {0}",
                // scrub the value stored in SP_EL0
                // "msr sp_el0, xzr",
                out(reg) sp,
                in(reg) spsel.get(),
            );
        }

        // make it so that the boot stack is in higher half memory
        if !VirtAddr::new(sp).unwrap().is_kernel() {
            unsafe {
                // we convert it to higher memory that has r/w permissions
                let new_sp = PhysAddr::new_unchecked(sp).kernel_vaddr().raw();
                core::arch::asm!(
                    "mov sp, {}",
                    in(reg) new_sp,
                );
            }
        }
    }
    // initialize the (local) settings for the interrupt controller
    init_interrupts();
}

pub fn start_clock(statclock_hz: u64, stat_cb: fn(Nanoseconds)) {
    crate::clock::stat::start(statclock_hz, stat_cb);
}

pub fn schedule_oneshot_tick(time: Nanoseconds) {
    let old = interrupt::disable();
    // set timer to fire off after a certian amount of time has passed
    let phys_timer = cntp::PhysicalTimer::new();
    let wait_time = TimeSpan::from_nanos(crate::clock::stat::clamp_oneshot(time));
    phys_timer.set_timer(wait_time);
    interrupt::set(old);
}

/// Jump into userspace
/// # Safety
/// The stack and target must be valid addresses.
pub unsafe fn jump_to_user(
    target: crate::memory::VirtAddr,
    stack: crate::memory::VirtAddr,
    arg: u64,
) {
    let ctx = syscall::Armv8SyscallContext::create_jmp_context(target, stack, arg);
    crate::thread::exit_kernel();
    syscall::return_to_user(&ctx);
}

/// QEMU exits 0 on PSCI SYSTEM_OFF; there is no isa-debug-exit here, so the code only reaches
/// the log and xtask judges the run by the test report.
pub fn debug_shutdown(code: u32) {
    log::info!("performing debug shutdown with code {}", code);
    let method = crate::machine::info::devicetree()
        .find_node("/psci")
        .and_then(|n| n.property("method"))
        .and_then(|p| p.as_str());
    let r = match method {
        Some("hvc") => smccc::psci::system_off::<smccc::Hvc>(),
        Some("smc") => smccc::psci::system_off::<smccc::Smc>(),
        _ => Err(smccc::psci::error::Error::NotSupported),
    };
    log::error!("debug shutdown did not power off: {:?}", r);
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}

/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(cpu: u32, tcb_base: crate::memory::VirtAddr, kernel_stack: *mut u8) {
    crate::machine::processor::poke_cpu(cpu, tcb_base, kernel_stack);
}
