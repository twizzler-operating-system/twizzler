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
#![feature(asm_sym)]
#![feature(asm_const)]

#[macro_use]
pub mod log;
pub mod arch;
mod clock;
mod idcounter;
mod image;
mod initrd;
mod interrupt;
pub mod machine;
pub mod memory;
mod mutex;
mod obj;
mod operations;
mod panic;
mod processor;
mod sched;
mod spinlock;
mod syscall;
mod thread;
pub mod utils;
extern crate alloc;

extern crate bitflags;
use arch::BootInfoSystemTable;
use initrd::BootModule;
use memory::MemoryRegion;
use spin::Once;
use thread::current_thread_ref;
use x86_64::VirtAddr;

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
}

fn kernel_main<B: BootInfo>(boot_info: &mut B) -> ! {
    let kernel_image_reg = 0xffffffff80000000u64;
    let clone_regions = [VirtAddr::new(kernel_image_reg)];
    arch::init(boot_info);
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
    arch::processor::enumerate_cpus();
    processor::init_cpu(image::get_tls());
    arch::init_secondary();
    initrd::init(boot_info.get_modules());
    processor::boot_all_secondaries(image::get_tls());

    clock::init();

    let lock = spinlock::Spinlock::<u32>::new(0);
    let mut v = lock.lock();
    *v = 2;

    thread::start_new_init();
    init_threading();
}

pub fn init_threading() -> ! {
    //arch::schedule_oneshot_tick(1000000000);
    //loop {}
    sched::create_idle_thread();
    clock::schedule_oneshot_tick(1);
    //thread::start_new(thread_main);
    //thread::start_new(thread_main);
    idle_main();
}

pub fn idle_main() -> ! {
    logln!(
        "processor {} entering main idle loop",
        current_processor().id
    );
    interrupt::set(true);
    loop {
        sched::schedule(true);
        arch::processor::halt_and_wait();
    }
}

#[allow(named_asm_labels)]
#[no_mangle]
#[naked]
unsafe extern "C" fn thread_user_main() {
    asm!(
        "ahah: mov rax, [0x1234]",
        "syscall",
        "jmp ahah",
        options(noreturn)
    );
}

static TEST: Once<mutex::Mutex<u32>> = Once::new();
extern "C" fn thread_main() {
    unsafe {
        arch::jump_to_user(
            VirtAddr::new(thread_user_main as usize as u64),
            VirtAddr::new(0),
            0,
        );
    }
    let thread = current_thread_ref().unwrap();
    TEST.call_once(|| mutex::Mutex::new(0));
    let test = TEST.wait();
    let mut i = 0u64;
    loop {
        // if i % 1000 == 0 {
        let _v = {
            let mut v = test.lock();
            *v += 1;
            *v
        };
        //  }
        i = i.wrapping_add(1);
        //let flags = x86_64::registers::rflags::read()
        //   .contains(x86_64::registers::rflags::RFlags::INTERRUPT_FLAG);
        //log!("{} {} {}\n", current_processor().id, thread.id(), flags);
        //logln!("{} {} {}", current_processor().id, thread.id(), v);
        //log!("{}", thread.id());
        if i % 100000 == 0 {
            logln!("{} {}", thread.id(), i);
        }
        // sched::schedule(true);
    }
}
