use core::sync::atomic::Ordering;

pub use x86_64::{VirtAddr, PhysAddr};

use crate::{
    clock::Nanoseconds,
    interrupt::{Destination, PinPolarity, TriggerMode},
    thread::current_thread_ref,
    BootInfo,
};

pub mod acpi;
pub mod address;
mod desctables;
pub mod interrupt;
pub mod ioapic;
pub mod lapic;
pub mod memory;
pub mod pagetables;
mod pit;
pub mod processor;
mod start;
mod syscall;
pub mod thread;
mod tsc;
pub use start::BootInfoSystemTable;
pub use lapic::{poke_cpu, send_ipi, schedule_oneshot_tick};
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

/// Jump into userspace
/// # Safety
/// The stack and target must be valid addresses.
pub unsafe fn jump_to_user(target: crate::memory::VirtAddr, stack: crate::memory::VirtAddr, arg: u64) {
    use crate::syscall::SyscallContext;
    let ctx = syscall::X86SyscallContext::create_jmp_context(target, stack, arg);
    crate::thread::exit_kernel();

    {
        /* we need this scope the drop the current thread ref before returning to user */
        let user_fs = current_thread_ref()
            .unwrap()
            .arch
            .user_fs
            .load(Ordering::SeqCst);
        x86::msr::wrmsr(x86::msr::IA32_FS_BASE, user_fs);
    }
    syscall::return_to_user(&ctx as *const syscall::X86SyscallContext);
}

pub fn set_interrupt(
    num: u32,
    masked: bool,
    trigger: TriggerMode,
    polarity: PinPolarity,
    destination: Destination,
) {
    ioapic::set_interrupt(num - 32, num, masked, trigger, polarity, destination);
}

pub fn debug_shutdown(code: u32) {
    unsafe {
        x86::io::outw(0xf4, code as u16);
    }
}

