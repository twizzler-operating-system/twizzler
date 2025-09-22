use core::sync::atomic::Ordering;

pub use address::{PhysAddr, VirtAddr};

use crate::{
    clock::Nanoseconds,
    interrupt::{Destination, PinPolarity, TriggerMode},
    thread::current_thread_ref,
    BootInfo,
};

pub mod acpi;
pub mod address;
mod apic;
pub mod context;
mod gdt;
pub mod image;
pub mod interrupt;
pub mod ioapic;
pub mod memory;
mod pit;
pub mod processor;
mod start;
mod syscall;
pub mod thread;
mod tsc;
pub use apic::{poke_cpu, send_ipi};
pub use start::BootInfoSystemTable;

use self::apic::get_lapic;
pub fn init(boot_info: &dyn BootInfo) {
    gdt::init();
    interrupt::init_idt();
    apic::init(true);

    let rsdp = boot_info.get_system_table(BootInfoSystemTable::Rsdp);
    acpi::init(rsdp.raw());
}

pub fn init_secondary() {
    gdt::init_secondary();
    interrupt::init_idt();
    apic::init(false);
}

pub fn init_interrupts() {
    ioapic::init()
}

pub fn start_clock(statclock_hz: u64, stat_cb: fn(Nanoseconds)) {
    pit::setup_freq(statclock_hz, stat_cb);
}

pub fn schedule_oneshot_tick(time: Nanoseconds) {
    get_lapic().setup_oneshot_timer(time)
}

/// Jump into userspace
/// # Safety
/// The stack and target must be valid addresses.
pub unsafe fn jump_to_user(
    target: crate::memory::VirtAddr,
    stack: crate::memory::VirtAddr,
    arg: u64,
) {
    use crate::syscall::SyscallContext;
    let ctx = syscall::X86SyscallContext::create_jmp_context(target, stack, arg);
    crate::interrupt::set(false);
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
    log::info!("performing debug shutdown with code {}", code);
    unsafe {
        x86::io::outw(0xf4, code as u16);
    }
}
