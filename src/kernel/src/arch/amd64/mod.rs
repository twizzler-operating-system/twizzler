use core::sync::atomic::Ordering;

use crate::{clock::Nanoseconds, thread::current_thread_ref, BootInfo};

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

/// Jump into userspace
/// # Safety
/// The stack and target must be valid addresses.
pub unsafe fn jump_to_user(target: VirtAddr, stack: VirtAddr, arg: u64) {
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
        x86_64::registers::segmentation::FS::write_base(VirtAddr::new(user_fs));
        x86::msr::wrmsr(x86::msr::IA32_FS_BASE, user_fs);
    }
    syscall::return_to_user(&ctx as *const syscall::X86SyscallContext);
}

pub use lapic::schedule_oneshot_tick;
use x86_64::{registers::segmentation::Segment64, VirtAddr};
