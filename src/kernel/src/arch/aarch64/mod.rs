use arm64::registers::{TPIDR_EL1, SPSel, SP_EL0};
use registers::interfaces::{Readable, Writeable};

use twizzler_abi::syscall::TimeSpan;

use crate::{
    clock::Nanoseconds,
    interrupt::{Destination, PinPolarity, TriggerMode},
    BootInfo,
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
pub use interrupt::{send_ipi, init_interrupts};
pub use start::BootInfoSystemTable;

pub fn init<B: BootInfo>(_boot_info: &B) {
    // initialize exceptions by setting up our exception vectors
    exception::init();
    // configure registers needed by the memory management system
    // TODO: configure MAIR

    // On reset, TPIDR_EL1 is initialized to some unknown value.
    // we set it to zero so that we know it is not initialized.
    TPIDR_EL1.set(0);

    // TODO: check if SPSel is already set to use SP_EL1
    // TODO: scrub SP_EL0 if we do change SP

    // make it so that we use SP_EL1 in the kernel
    // when taking an exception.
    SPSel.write(SPSel::SP::ELx);

    // save the stack pointer from before
    let old_sp = SP_EL0.get();

    // make it so that the boot stack is in higher half memory
    //
    // NOTE: this is currently specific to Limine on aarch64
    let sp = if VirtAddr::new(old_sp).unwrap().is_kernel() {
        old_sp
    } else {
        unsafe {
            PhysAddr::new_unchecked(old_sp).kernel_vaddr().raw()
        }
    };

    // set current stack pointer to previous,
    // sp is now aliased to SP_EL1
    unsafe {
        core::arch::asm!(
            "mov sp, {}",
            in(reg) sp,
        );
    }
}

pub fn init_secondary() {
    // TODO: Initialize secondary processors:
    // - set up exception handling
    // - configure the local CPU interrupt controller interface
}

pub fn set_interrupt(
    _num: u32,
    _masked: bool,
    _trigger: TriggerMode,
    _polarity: PinPolarity,
    _destination: Destination,
) {
    todo!();
}

pub fn start_clock(_statclock_hz: u64, _stat_cb: fn(Nanoseconds)) {
    // TODO: implement support for the stat clock
}

pub fn schedule_oneshot_tick(time: Nanoseconds) {
    emerglogln!("[arch::tick] setting the timer to fire off after {} ns", time);
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
pub unsafe fn jump_to_user(_target: crate::memory::VirtAddr, _stack: crate::memory::VirtAddr, _arg: u64) {
    todo!("jump to user");
}

pub fn debug_shutdown(_code: u32) {
    todo!()
}

/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(_cpu: u32, _tcb_base: crate::memory::VirtAddr, _kernel_stack: *mut u8) {
    todo!("start up a cpu")
}
