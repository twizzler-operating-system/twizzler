#![no_std]
#![no_main]
#![allow(internal_features)]
#![feature(alloc_error_handler)]
#![feature(thread_local)]
#![feature(exclusive_range_pattern)]
#![feature(naked_functions)]
#![allow(dead_code)]
#![feature(core_intrinsics)]
#![feature(optimize_attribute)]
#![feature(lang_items)]
#![feature(asm_const)]
#![feature(custom_test_frameworks)]
#![reexport_test_harness_main = "test_main"]
#![test_runner(crate::test_runner)]
#![feature(stmt_expr_attributes)]
#![feature(int_roundings)]
#![feature(const_option)]
#![feature(let_chains)]
#![feature(btree_extract_if)]

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
mod pager;
mod panic;
mod processor;
mod queue;
mod sched;
mod spinlock;
mod syscall;
mod thread;
mod time;
mod userinit;
pub mod utils;
extern crate alloc;

extern crate bitflags;

#[macro_use]
pub mod debug;

use core::sync::atomic::{AtomicBool, Ordering};

use arch::BootInfoSystemTable;
use initrd::BootModule;
use memory::{MemoryRegion, VirtAddr};

use crate::{processor::current_processor, thread::entry::start_new_init};

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
    // arch::init(boot_info);
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

    // AA: initialize physical frame allocation before calling test functions 
    crate::memory::frame::init(boot_info.memory_regions());
    
    // machine::tests::test_terminal(0x1);

    // // test with limine's pt entries
    // machine::tests::print_page_table(crate::memory::pagetables::Table::current(), 0);

    // machine::tests::test_terminal(0x2);
    
    // machine::tests::test_map_limine();
    
    // machine::tests::test_terminal(0x3);

    machine::tests::test_limine_serial();
    machine::tests::test_uart_echo();

    // let's try to log something
    // "initialize" the memory init flag
    // this is for testing logging only and should be removed
    memory::MEM_INIT.store(true, Ordering::SeqCst);
    log!("A");
    machine::tests::test_terminal(0x4);
    machine::tests::test_uart_echo();
    machine::tests::test_terminal(0x5);
    
    loop{}

    terminal!("[kernel::mm] initializing memory management");
    memory::init(boot_info);

    logln!("[kernel::debug] parsing kernel debug image");
    let (kernel_image_start, kernel_image_length) = boot_info.kernel_image_info();
    unsafe {
        let kernel_image =
            core::slice::from_raw_parts(kernel_image_start.as_ptr(), kernel_image_length);
        image::init(kernel_image);
        panic::init(kernel_image);
    }

    arch::init_interrupts();

    logln!("[kernel::cpu] enumerating secondary CPUs");
    let bsp_id = arch::processor::enumerate_cpus();
    processor::init_cpu(image::get_tls(), bsp_id);
    arch::init_secondary();
    initrd::init(boot_info.get_modules());
    logln!("[kernel::cpu] booting secondary CPUs");
    processor::boot_all_secondaries(image::get_tls());

    clock::init();
    interrupt::init();

    let lock = spinlock::Spinlock::<u32>::new(0);
    let mut v = lock.lock();
    *v = 2;

    init_threading();
}

#[cfg(test)]
pub fn test_runner(tests: &[&(&str, &dyn Fn())]) {
    logln!(
        "[kernel::test] running {} tests, test thread ID: {}",
        tests.len(),
        crate::thread::current_thread_ref().unwrap().id()
    );
    for test in tests {
        log!("test {} ... ", test.0);
        (test.1)();
        logln!("ok");
        if !interrupt::get() {
            panic!("test {} didn't cleanup interrupt state", test.0);
        }
    }

    logln!("[kernel::test] test result: ok.");
}

pub fn init_threading() -> ! {
    sched::create_idle_thread();
    clock::schedule_oneshot_tick(1);
    idle_main();
}

pub fn idle_main() -> ! {
    interrupt::set(true);
    if current_processor().is_bsp() {
        machine::machine_post_init();

        #[cfg(test)]
        if is_test_mode() {
            // Run tests on a high priority thread, so any threads spawned by tests
            // don't preempt the testing thread.
            crate::thread::entry::run_closure_in_new_thread(
                crate::thread::priority::Priority::default_realtime(),
                || test_main(),
            )
            .1
            .wait(true);
        }
        start_new_init();
    }
    logln!(
        "[kernel::main] processor {} entering main idle loop",
        current_processor().id
    );
    loop {
        sched::schedule(true);
        arch::processor::halt_and_wait();
    }
}
