#![no_std]
#![no_main]
#![allow(internal_features)]
#![feature(alloc_error_handler)]
#![feature(thread_local)]
#![allow(dead_code)]
#![feature(core_intrinsics)]
#![feature(optimize_attribute)]
#![feature(lang_items)]
#![feature(custom_test_frameworks)]
#![reexport_test_harness_main = "test_main"]
#![test_runner(crate::test_runner)]
#![feature(stmt_expr_attributes)]
#![feature(int_roundings)]
#![feature(allocator_api)]
#![feature(likely_unlikely)]
#![feature(ptr_as_ref_unchecked)]
#![feature(atomic_internals)]

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
pub mod security;
mod spinlock;
mod syscall;
mod thread;
mod time;
mod trace;
mod userinit;
pub mod utils;
extern crate alloc;

extern crate bitflags;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ::log::LevelFilter;
use arch::BootInfoSystemTable;
use initrd::BootModule;
use memory::{MemoryRegion, VirtAddr};
use once::Once;
use processor::{
    mp::{boot_all_secondaries, init_cpu},
    sched::{SchedFlags, schedule},
};
use random::start_entropy_contribution_thread;
use syscall::sync::requeue_all;

use crate::{
    arch::PhysAddr,
    obj::scan_deleted,
    pager::check_timed_out_requests,
    processor::mp::current_processor,
    thread::{check_orphan_threads, entry::start_new_init, locktrack::check_timed_out_mutexes},
};

/// A collection of information made available to the kernel by the bootloader or arch-dep modules.
pub trait BootInfo {
    /// Return a static array of memory regions for the system.
    fn memory_regions(&self) -> &'static [MemoryRegion];
    /// Return the address and length of the whole kernel image.
    fn kernel_image_info(&self) -> (VirtAddr, usize);
    /// Get a system table, the kinds available depend on the platform and architecture.
    fn get_system_table(&self, table: BootInfoSystemTable) -> PhysAddr;
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

static NO_PCID: AtomicBool = AtomicBool::new(false);
/// Whether the boot cmdline asked us to run without PCIDs (x86_64). A kill switch for bisecting
/// TLB coherence problems: with this set, address space switches flush as they always did.
pub fn no_pcid() -> bool {
    NO_PCID.load(Ordering::SeqCst)
}

static DIAG_MODE: AtomicBool = AtomicBool::new(false);
/// Run the idle-loop hang diagnostics outside of test mode.
///
/// They are unconditional under `--tests`, which meant an `--autostart` run -- the only way to
/// drive a workload like `pagepar` -- had no timeout checker, no orphan-thread scan and no hang
/// report at all. A stall there looked silent because nothing was watching, not because nothing
/// was stuck.
pub fn is_diag_mode() -> bool {
    DIAG_MODE.load(Ordering::SeqCst)
}

static BOOT_INFO: Once<Box<dyn BootInfo + Send + Sync>> = Once::new();

pub fn get_boot_info() -> &'static dyn BootInfo {
    &**BOOT_INFO.poll().unwrap()
}

struct Logger {}

impl ::log::Log for Logger {
    fn enabled(&self, _metadata: &::log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &::log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        // most messages come from the kernel, so keep it short.
        let target = record
            .target()
            .strip_prefix("twizzler_")
            .unwrap_or(record.target());

        logln!("[{}] {} -- {}", target, record.level(), record.args(),);
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger {};

fn kernel_main<B: BootInfo + Send + Sync + 'static>(boot_info: B) -> ! {
    let boot_info = &**BOOT_INFO.call_once(|| Box::new(boot_info));
    arch::init();
    ::log::set_logger(&LOGGER).unwrap();
    ::log::set_max_level(LevelFilter::Info);
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
        if opt == "--no-pcid" {
            NO_PCID.store(true, Ordering::SeqCst);
        }
        if opt == "--diag" {
            DIAG_MODE.store(true, Ordering::SeqCst);
        }
    }

    if is_test_mode() {
        logln!("!!! TEST MODE ACTIVE");
    }
    // Before memory::init, which builds the kernel context: a context's PCID is fixed when it is
    // constructed.
    #[cfg(target_arch = "x86_64")]
    arch::processor::init_pcid();
    logln!("[kernel::mm] initializing memory management");
    memory::init(boot_info);
    arch::init_post_memory(boot_info);

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
    init_cpu(image::get_tls(), bsp_id);
    arch::init_interrupts();
    #[cfg(target_arch = "x86_64")]
    arch::init_secondary();
    ::log::set_max_level(LevelFilter::Off);
    initrd::init(boot_info.get_modules());
    ::log::set_max_level(LevelFilter::Info);
    logln!("[kernel::cpu] booting secondary CPUs");
    boot_all_secondaries(image::get_tls());

    clock::init();
    interrupt::init();

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
        logln!("starting test {}", test.0);
        (test.1)();
        logln!("test {}: ok", test.0);
        if !interrupt::get() {
            panic!("test {} didn't cleanup interrupt state", test.0);
        }
    }

    logln!("[kernel::test] test result: ok.");
}

pub fn init_threading() -> ! {
    processor::sched::create_idle_thread();
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
    let mut iter = 0u32;
    loop {
        if iter % 100 == 0 {
            current_processor().cleanup_exited();
        }
        if iter % 1000 == 0 && current_processor().is_bsp() {
            scan_deleted();
            // The rest are diagnostics, and they are not free: each walks every thread or every
            // inflight request under that structure's lock, from the idle loop, on every scan.
            // `check_system_hang` additionally reports on threads that are merely idle -- service
            // threads parked on a condvar cross its 25s threshold in every boot, which spent its
            // whole report budget on healthy runs. Restricted to test mode, where a sweep is
            // reading the transcript and the cost buys something -- or to an explicit `--diag`,
            // for an autostart run that is being debugged.
            if is_test_mode() || is_diag_mode() {
                check_timed_out_mutexes();
                check_timed_out_requests();
                check_orphan_threads();
                crate::thread::check_system_hang();
                crate::obj::promotion_census();
            }
        }
        iter = iter.wrapping_add(1);
        requeue_all();
        schedule(SchedFlags::REINSERT | SchedFlags::YIELD | SchedFlags::PREEMPT);
        requeue_all();
        arch::processor::halt_and_wait();
    }
}
