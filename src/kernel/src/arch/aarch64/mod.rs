use arm64::registers::{TPIDR_EL1, SPSel};
use registers::{
    registers::InMemoryRegister,
    interfaces::{Readable, Writeable},
};

use twizzler_abi::syscall::TimeSpan;

use crate::{
    clock::Nanoseconds,
    BootInfo,
    syscall::SyscallContext,
};

pub mod address;
mod cntp;
pub mod context;
mod exception;
pub mod image;
pub mod interrupt;
pub mod memory;
pub mod processor;
mod syscall;
pub mod thread;
mod start;

pub use address::{VirtAddr, PhysAddr};
pub use interrupt::{send_ipi, init_interrupts, set_interrupt};
pub use start::BootInfoSystemTable;

pub fn init<B: BootInfo>(boot_info: &B) {
    // initialize exceptions by setting up our exception vectors
    exception::init();
    // configure registers needed by the memory management system
    // TODO: configure MAIR

    // On reset, TPIDR_EL1 is initialized to some unknown value.
    // we set it to zero so that we know it is not initialized.
    TPIDR_EL1.set(0);

    // Initialize the machine specific enumeration state (e.g., DeviceTree, ACPI)
    crate::machine::info::init(boot_info);
    
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
}

pub fn init_secondary() {
    // initialize exceptions by setting up our exception vectors
    exception::init();
    
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

pub fn start_clock(_statclock_hz: u64, _stat_cb: fn(Nanoseconds)) {
    // TODO: implement support for the stat clock
}

pub fn schedule_oneshot_tick(time: Nanoseconds) {
    let old = interrupt::disable();
    // set timer to fire off after a certian amount of time has passed
    let phys_timer = cntp::PhysicalTimer::new();
    let wait_time = TimeSpan::from_nanos(time);
    phys_timer.set_timer(wait_time);
    interrupt::set(old);
}

/// Jump into userspace
/// # Safety
/// The stack and target must be valid addresses.
pub unsafe fn jump_to_user(target: crate::memory::VirtAddr, stack: crate::memory::VirtAddr, arg: u64) {
    let ctx = syscall::Armv8SyscallContext::create_jmp_context(target, stack, arg);
    crate::thread::exit_kernel();
    syscall::return_to_user(&ctx);
}

pub fn debug_shutdown(_code: u32) {
    todo!()
}

/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(cpu: u32, tcb_base: crate::memory::VirtAddr, kernel_stack: *mut u8) {
    crate::machine::processor::poke_cpu(cpu, tcb_base, kernel_stack);
}
