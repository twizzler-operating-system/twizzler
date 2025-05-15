#![no_std]
#![no_main]
#![allow(internal_features)]
#![feature(alloc_error_handler)]
#![feature(thread_local)]
#![feature(naked_functions)]
#![allow(dead_code)]
#![feature(core_intrinsics)]
#![feature(optimize_attribute)]
#![feature(lang_items)]
#![feature(custom_test_frameworks)]
#![reexport_test_harness_main = "test_main"]
#![test_runner(crate::test_runner)]
#![feature(stmt_expr_attributes)]
#![feature(int_roundings)]
#![feature(let_chains)]
#![feature(btree_extract_if)]
#![feature(allocator_api)]

#[macro_use]
pub mod log;
pub mod arch;
mod clock;
mod condvar;
mod crypto;
mod device;
mod idcounter;
mod image;
mod initrd;
mod instant;
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
mod random;
mod sched;
pub mod security;
mod spinlock;
mod syscall;
mod thread;
mod time;
mod userinit;
pub mod utils;
extern crate alloc;

extern crate bitflags;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ::log::Level;
use arch::BootInfoSystemTable;
use initrd::BootModule;
use memory::{MemoryRegion, VirtAddr};
use once::Once;
use random::start_entropy_contribution_thread;

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

const BENCH_MODE_ALL: u32 = 1;
const BENCH_MODE_USER: u32 = 2;
static BENCH_MODE: AtomicU32 = AtomicU32::new(0);
pub fn is_bench_mode() -> bool {
    BENCH_MODE.load(Ordering::SeqCst) > 0
}

static BOOT_INFO: Once<Box<dyn BootInfo + Send + Sync>> = Once::new();

pub fn get_boot_info() -> &'static dyn BootInfo {
    &**BOOT_INFO.poll().unwrap()
}

struct Logger {}

impl ::log::Log for Logger {
    fn enabled(&self, metadata: &::log::Metadata) -> bool {
        metadata.level() <= Level::Trace
    }

    fn log(&self, record: &::log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        logln!(
            "{}:{} -- {}",
            record.level(),
            record.target(),
            record.args()
        );
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger {};

fn kernel_main<B: BootInfo + Send + Sync + 'static>(boot_info: B) -> ! {
    ::log::set_logger(&LOGGER).unwrap();
    let boot_info = &**BOOT_INFO.call_once(|| Box::new(boot_info));
    arch::init(boot_info);
    logln!("[kernel] boot with cmd `{}'", boot_info.get_cmd_line());
    ::log::warn!("TEST LOG");
    let cmdline = boot_info.get_cmd_line();
    for opt in cmdline.split(" ") {
        if opt == "--tests" {
            TEST_MODE.store(true, Ordering::SeqCst);
        }
        if opt == "--benches" {
            BENCH_MODE.store(BENCH_MODE_ALL, Ordering::SeqCst);
        }
        if opt == "--bench" {
            BENCH_MODE.store(BENCH_MODE_USER, Ordering::SeqCst);
        }
    }

    if is_test_mode() {
        logln!("!!! TEST MODE ACTIVE");
    }
    logln!("[kernel::mm] initializing memory management");
    memory::init(boot_info);

    logln!("[kernel::debug] parsing kernel debug image");
    let (kernel_image_start, kernel_image_length) = boot_info.kernel_image_info();
    unsafe {
        let kernel_image =
            core::slice::from_raw_parts(kernel_image_start.as_ptr(), kernel_image_length);
        image::init(kernel_image);
        panic::init(kernel_image);
    }

    logln!("[kernel::cpu] enumerating secondary CPUs");
    let bsp_id = arch::processor::enumerate_cpus();
    processor::init_cpu(image::get_tls(), bsp_id);
    arch::init_interrupts();
    #[cfg(target_arch = "x86_64")]
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
        start_entropy_contribution_thread();

        #[cfg(test)]
        if is_test_mode() {
            // Run tests on a high priority thread, so any threads spawned by tests
            // don't preempt the testing thread.
            crate::thread::entry::run_closure_in_new_thread(
                crate::thread::priority::Priority::REALTIME,
                || test_main(),
            )
            .1
            .wait();
        }
        start_new_init();
    }
    logln!(
        "[kernel::main] processor {} entering main idle loop",
        current_processor().id
    );
    loop {
        {
            current_processor().cleanup_exited();
        }
        sched::schedule(true);
        arch::processor::halt_and_wait();
    }
}
