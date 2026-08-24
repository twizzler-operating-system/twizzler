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
mod perfmark;
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
use core::{
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Duration,
};

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
    condvar::CondVar,
    obj::scan_deleted,
    pager::check_timed_out_requests,
    processor::mp::current_processor,
    syscall::sync::sys_thread_sync,
    thread::{
        check_orphan_threads, entry::start_new_init, locktrack::check_timed_out_mutexes,
        priority::Priority,
    },
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

/// The reaper thread, on unless `--reap=legacy`.
///
/// Default-on as of 2026-08-20, after leak30/leak31 measured the legacy paths pinning 2.54 GiB of
/// kernel stacks under a 2,200-spawn workload with `trk.reclaiming` at zero throughout. Reaping
/// without this thread is gated on a stattick landing in user mode or a hundredth idle pass, both
/// anti-correlated with the churn that produces the backlog; with it, `thr.exited_backlog` holds at
/// slope zero and `thr.reaped` tracks production exactly.
///
/// `--reap=legacy` restores the old behaviour, and remains a runtime knob rather than a const so an
/// A/B can be run from one build and one tree state.
static REAP_THREAD: AtomicBool = AtomicBool::new(true);

pub fn reap_thread_enabled() -> bool {
    REAP_THREAD.load(Ordering::SeqCst)
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
    logln!(
        "[kernel] lock sizes: spinlock<()> {} spinlock<u64> {} mutex<()> {} mutex<u64> {} object {} thread {}",
        core::mem::size_of::<crate::spinlock::Spinlock<()>>(),
        core::mem::size_of::<crate::spinlock::Spinlock<u64>>(),
        core::mem::size_of::<crate::mutex::Mutex<()>>(),
        core::mem::size_of::<crate::mutex::Mutex<u64>>(),
        core::mem::size_of::<crate::obj::Object>(),
        core::mem::size_of::<crate::thread::Thread>(),
    );
    // Every A/B arm on the fault path is a const, and `many.py`'s source fingerprint can only say
    // the tree did not change -- not that this image was built from it. A line the arm prints for
    // itself is the check that survives a stale target dir or a peer's concurrent build.
    logln!(
        "[kernel] fault tunables: fault_around {} fill_batch {}/{} large_anon {} fault_profile {} slot_memo {}",
        crate::memory::context::virtmem::region::ANON_FAULT_AROUND,
        crate::obj::data::FILL_BATCH,
        crate::obj::data::FILL_BATCH_MAX,
        crate::obj::data::TRY_LARGE_ANON_PAGES,
        crate::memory::context::virtmem::fault::FAULT_PROFILE,
        crate::memory::context::virtmem::fault::FAULT_SLOT_MEMO,
    );
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
            memory::frame::enable_pt_zero_check();
        }
        if opt == "--pt-zero-check" {
            memory::frame::enable_pt_zero_check();
        }
        if opt == "--reap=legacy" {
            REAP_THREAD.store(false, Ordering::SeqCst);
        }
        if opt == "--kalloc-census" {
            memory::kalloc_census::enable();
        }
        if let Some(spec) = opt.strip_prefix("--kalloc-trap=") {
            memory::kalloc_census::set_trap(spec);
        }
    }

    if is_test_mode() {
        logln!("!!! TEST MODE ACTIVE");
    }
    if is_diag_mode() {
        thread::log_thread_sizes();
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

static BG_ZERO_CV: CondVar = CondVar::new();
// TODO: wake this on an actual condition.
static BG_ZERO_SPINLOCK: spinlock::Spinlock<()> = spinlock::Spinlock::new(());

extern "C" fn background_worker() {
    loop {
        // Frame cache first: its trim is what returns cached memory under pressure, and its
        // zeroing feeds the path that would otherwise memset inline on a fault. Both are cheap
        // no-ops when the cache is disabled or has nothing to do, so this costs an atomic load
        // per pass in the arm where it is off.
        if memory::framecache::service() {
            processor::sched::schedule(
                SchedFlags::REINSERT | SchedFlags::YIELD | SchedFlags::PREEMPT,
            );
            continue;
        }
        if !memory::frame::background_zero_iter() {
            let guard = BG_ZERO_SPINLOCK.lock();
            let _ = BG_ZERO_CV.wait(guard);
        } else {
            processor::sched::schedule(
                SchedFlags::REINSERT | SchedFlags::YIELD | SchedFlags::PREEMPT,
            );
        }
    }
}

/// Boot sequencing, on its own thread.
///
/// This used to run on the bsp's idle thread, which `wait()`ed for the tests here -- ahead of the
/// idle loop, where every hang diagnostic lives behind `is_bsp()`. So for the whole kernel-test
/// phase `check_orphan_threads`, `check_system_hang` and `check_timed_out_mutexes` were all
/// switched off, and that is the window in which a test that wedges the system wedges it: the
/// transcript simply stops, because nothing was left running that could describe it. `bsp_watchdog`
/// does not cover it either -- it needs the bsp's tick to stall, and a bsp spinning in that wait
/// with interrupts on keeps ticking.
///
/// REALTIME to preserve the old property that threads a test spawns do not preempt the test thread,
/// and the order below is the order the idle thread ran these in.
extern "C" fn boot_sequence() {
    #[cfg(test)]
    if is_test_mode() {
        test_main();
    }
    start_new_init();
    let _ = crate::thread::entry::start_new_kernel(Priority::BACKGROUND, background_worker, 0);
    if reap_thread_enabled() {
        crate::thread::reaper::start();
    }
    crate::thread::exit(0);
}

/// A/B knob for the schedmon perturbation question: besides its 30s wake, schedmon's timeout
/// entry keeps a wheel window occupied, so `hard_advance` signals the INTERRUPT-priority timeout
/// thread roughly once per second for the whole boot -- a scheduling perturbation inside the very
/// subsystem the release-smp1 wedge lives in. Arm B builds with this false.
const SCHEDMON_ENABLED: bool = true;

/// Spawn the scheduler monitor: `schedmon_dump` every 30s from a REALTIME thread, so it keeps
/// reporting when a spinning USER thread starves the idle loop (which is where every other hang
/// diagnostic lives). Sleeps on the timeout queue, so a transcript whose `[schedmon]` heartbeat
/// *stops* additionally says the tick machinery died.
fn start_schedmon() {
    let _ = crate::thread::entry::run_closure_in_new_thread(
        crate::thread::priority::Priority::REALTIME,
        || {
            logln!("[schedmon] armed");
            let mut pass = 0u64;
            loop {
                let _ = crate::syscall::sync::sys_thread_sync(
                    &mut [],
                    Some(&mut core::time::Duration::from_secs(30)),
                );
                pass += 1;
                crate::processor::sched::schedmon_dump(pass);
            }
        },
    );
}

pub fn idle_main() -> ! {
    interrupt::set(true);
    if current_processor().is_bsp() {
        machine::machine_post_init();
        start_entropy_contribution_thread();
        if SCHEDMON_ENABLED && (is_test_mode() || is_diag_mode()) {
            start_schedmon();
        }

        let _ = crate::thread::entry::start_new_kernel(
            crate::thread::priority::Priority::REALTIME,
            boot_sequence,
            0,
        );
    }
    logln!(
        "[kernel::main] processor {} entering main idle loop",
        current_processor().id
    );
    let mut iter = 0u32;
    loop {
        // Deliver wakeups parked on the requeue list. A signal that lands between a waiter's
        // setup_wait and its finish_blocking finds the waiter still critical and defers the
        // wakeup (add_to_requeue); any later requeue_all delivers it, but a system quiescing at
        // that moment never runs one and the waiter sleeps forever on an idle machine. The cpu
        // that ran the waiter reaches this loop right after it blocks, so draining here bounds
        // the loss to one idle pass. Cheap when empty: requeue_all early-outs on the count.
        crate::syscall::sync::requeue_all();
        // Covers the case the stattick safe-point reap structurally cannot: a cpu with nothing in
        // user mode to interrupt. One relaxed load when there is nothing to do.
        crate::thread::reaper::notify();
        if iter % 100 == 0 {
            current_processor().cleanup_exited();
        }
        if iter % 1000 == 0 && current_processor().is_bsp() {
            BG_ZERO_CV.signal();
            scan_deleted();
            // The rest are diagnostics, and they are not free: each walks every thread or every
            // inflight request under that structure's lock, from the idle loop, on every scan.
            // `check_system_hang` additionally reports on threads that are merely idle -- service
            // threads parked on a condvar cross its 25s threshold in every boot. They no longer
            // spend the whole report budget doing it (see `MAX_THREAD_HANG_REPORTS`), but they do
            // still cost a table each. Restricted to test mode, where a sweep is reading the
            // transcript and the cost buys something -- or to an explicit `--diag`, for an
            // autostart run that is being debugged.
            if is_test_mode() || is_diag_mode() {
                check_timed_out_mutexes();
                check_timed_out_requests();
                check_orphan_threads();
                crate::thread::check_system_hang();
                crate::obj::promotion_census();
                crate::processor::report_exited_backlog();
            }
        }
        // The same diagnostics, from a cpu that still can, when the bsp has stopped ticking. See
        // `clock::bsp_watchdog`: everything above is gated on `is_bsp()`, so a bsp spinning with
        // interrupts masked silences the entire diagnostic surface -- which is the one state it
        // most needs to describe. Fires at most once per boot.
        if iter % 1000 == 0
            && !current_processor().is_bsp()
            && (is_test_mode() || is_diag_mode())
            && crate::clock::bsp_watchdog::stalled()
        {
            emerglogln!(
                "[watchdog] bsp tick has stopped advancing; reporting from cpu {}",
                current_processor().id
            );
            // Cheap and lock-free first, because everything after this takes a lock the wedged cpu
            // may be holding, and a watchdog that hangs before printing is no watchdog.
            crate::thread::locktrack::diag::print_counters(true);
            crate::thread::check_system_hang();
            check_timed_out_mutexes();
            check_orphan_threads();
            emerglogln!("[watchdog] end of report");
        }
        iter = iter.wrapping_add(1);
        requeue_all();
        schedule(SchedFlags::REINSERT | SchedFlags::YIELD | SchedFlags::PREEMPT);
        requeue_all();
        arch::processor::halt_and_wait();
    }
}
