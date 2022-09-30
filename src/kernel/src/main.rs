#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(thread_local)]
#![feature(asm)]
#![feature(global_asm)]
#![feature(exclusive_range_pattern)]
#![feature(naked_functions)]
#![allow(dead_code)]
#![feature(map_first_last)]
#![feature(const_fn_trait_bound)]
#![feature(core_intrinsics)]
#![feature(derive_default_enum)]
#![feature(const_btree_new)]
#![feature(optimize_attribute)]
#![feature(lang_items)]
#![feature(btree_drain_filter)]
#![feature(custom_test_frameworks)]
#![reexport_test_harness_main = "test_main"]
#![test_runner(crate::test_runner)]

#[macro_use]
pub mod log;
pub mod arch;
mod clock;
mod condvar;
mod device;
mod idcounter;
mod image;
mod initrd;
mod interrupt;
pub mod machine;
pub mod memory;
mod mutex;
mod obj;
mod once;
mod operations;
mod panic;
mod processor;
mod sched;
mod spinlock;
mod syscall;
mod thread;
mod time;
pub mod utils;
extern crate alloc;

extern crate bitflags;
use core::sync::atomic::{AtomicBool, Ordering};

use arch::BootInfoSystemTable;
use initrd::BootModule;
use memory::{MemoryRegion, VirtAddr};

use crate::processor::current_processor;

/// A collection of information made available to the kernel by the bootloader or arch-dep modules.
pub trait BootInfo {
    /// Return a static array of memory regions for the system.
    fn memory_regions(&self) -> &'static [MemoryRegion];
    /// Return the address and length of the whole kernel image.
    fn kernel_image_info(&self) -> (VirtAddr, usize);
    /// Get a system table, the kinds available depend on the platform and architecture.
    fn get_system_table(&self, table: BootInfoSystemTable) -> VirtAddr;
    /// Get a static array of the modules loaded by the bootloader
    fn get_modules(&self) -> &'static [BootModule];
    /// Get a pointer to the kernel command line.
    fn get_cmd_line(&self) -> &'static str;
}

static TEST_MODE: AtomicBool = AtomicBool::new(false);
pub fn is_test_mode() -> bool {
    TEST_MODE.load(Ordering::SeqCst)
}

fn kernel_main<B: BootInfo>(boot_info: &mut B) -> ! {
    let kernel_image_reg = 0xffffffff80000000u64;
    let clone_regions = [VirtAddr::new(kernel_image_reg)];
    arch::init(boot_info);
    logln!("[kernel] boot with cmd `{}'", boot_info.get_cmd_line());
    let cmdline = boot_info.get_cmd_line();
    for opt in cmdline.split(" ") {
        if opt == "--tests" {
            TEST_MODE.store(true, Ordering::SeqCst);
        }
    }

    if is_test_mode() {
        logln!("!!! TEST MODE ACTIVE");
    }
    logln!("[kernel::mm] initializing memory management");
    memory::init(boot_info, &clone_regions);

    logln!("[kernel::debug] parsing kernel debug image");
    let (kernel_image_start, kernel_image_length) = boot_info.kernel_image_info();
    unsafe {
        let kernel_image =
            core::slice::from_raw_parts(kernel_image_start.as_ptr(), kernel_image_length);
        image::init(kernel_image);
        panic::init(kernel_image);
    }

    arch::init_interrupts();

    logln!("[kernel::cpu] enumerating and starting secondary CPUs");
    let bsp_id = arch::processor::enumerate_cpus();
    processor::init_cpu(image::get_tls(), bsp_id);
    arch::init_secondary();
    initrd::init(boot_info.get_modules());
    processor::boot_all_secondaries(image::get_tls());

    clock::init();
    interrupt::init();

    let lock = spinlock::Spinlock::<u32>::new(0);
    let mut v = lock.lock();
    *v = 2;

    init_threading();
}

#[cfg(test)]
pub fn test_runner(tests: &[&dyn Fn()]) {
    logln!("[kernel::test] running {} tests", tests.len());
    for test in tests {
        test();
    }

    logln!("[kernel::test] test result: ok.");
}

pub fn init_threading() -> ! {
    sched::create_idle_thread();
    clock::schedule_oneshot_tick(1);
    idle_main();
}

pub fn idle_main() -> ! {
    if current_processor().is_bsp() {
        machine::machine_post_init();

        #[cfg(test)]
        if is_test_mode() {
            test_main();
        }
        thread::start_new_init();
    }
    logln!(
        "[kernel::main] processor {} entering main idle loop",
        current_processor().id
    );
    interrupt::set(true);
    loop {
        sched::schedule(true);
        arch::processor::halt_and_wait();
    }
}
