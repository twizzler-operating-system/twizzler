use crate::{clock::Nanoseconds, BootInfo};

pub mod acpi;
mod desctables;
pub mod interrupt;
pub mod ioapic;
pub mod lapic;
pub mod memory;
mod pit;
pub mod processor;
mod start;
mod syscall;
pub mod thread;
pub use start::BootInfoSystemTable;
pub fn init<B: BootInfo>(boot_info: &B) {
    desctables::init();
    interrupt::init_idt();
    lapic::init(true);

    let rsdp = boot_info.get_system_table(BootInfoSystemTable::Rsdp);
    acpi::init(rsdp.as_u64());
}

pub fn init_secondary() {
    desctables::init_secondary();
    interrupt::init_idt();
    lapic::init(false);
}

pub fn init_interrupts() {
    ioapic::init()
}

pub fn start_clock(statclock_hz: u64, stat_cb: fn(Nanoseconds)) {
    pit::setup_freq(statclock_hz, stat_cb);
}

pub unsafe fn jump_to_user(target: VirtAddr, stack: VirtAddr, arg: u64) {
    use crate::syscall::SyscallContext;
    let ctx = syscall::X86SyscallContext::create_jmp_context(target, stack, arg);
    crate::thread::exit_kernel();
    syscall::return_to_user(&ctx as *const syscall::X86SyscallContext);
}

pub use lapic::schedule_oneshot_tick;
use x86_64::VirtAddr;
