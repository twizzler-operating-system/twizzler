use crate::{
    clock::Nanoseconds,
    interrupt::{Destination, PinPolarity, TriggerMode},
    BootInfo,
};

pub mod address;
pub mod context;
pub mod interrupt;
pub mod memory;
pub mod processor;
mod syscall;
pub mod thread;
mod start;

pub use address::{VirtAddr, PhysAddr};
pub use interrupt::send_ipi;
pub use start::BootInfoSystemTable;

pub fn kernel_main() -> ! {
    emerglogln!("[kernel] hello world!!");
    loop {}
}

pub fn init<B: BootInfo>(_boot_info: &B) {
    todo!();
}

pub fn init_secondary() {
    todo!();
}

pub fn init_interrupts() {
    todo!()
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
    todo!();
}

pub fn schedule_oneshot_tick(_time: Nanoseconds) {
    todo!()
}

/// Jump into userspace
/// # Safety
/// The stack and target must be valid addresses.
pub unsafe fn jump_to_user(_target: crate::memory::VirtAddr, _stack: crate::memory::VirtAddr, _arg: u64) {
    todo!();
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
