#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(thread_local)]
#![feature(asm)]
#![feature(global_asm)]
#![feature(exclusive_range_pattern)]
#![feature(naked_functions)]
//#![feature(asm_const)]
//#![feature(asm_sym)]
#![allow(dead_code)]
#![feature(map_first_last)]
#![feature(const_fn_trait_bound)]
#![feature(core_intrinsics)]
#![feature(derive_default_enum)]
#![feature(const_btree_new)]

#[macro_use]
pub mod log;
pub mod arch;
mod clock;
mod image;
mod initrd;
mod interrupt;
pub mod machine;
mod memory;
mod mutex;
mod panic;
mod processor;
mod sched;
mod spinlock;
mod thread;
pub mod utils;
extern crate alloc;

extern crate bitflags;
use arch::BootInfoSystemTable;
use memory::MemoryRegion;
use spin::Once;
use thread::current_thread_ref;
use x86_64::VirtAddr;

use crate::processor::current_processor;

pub trait BootInfo {
    fn memory_regions(&self) -> &'static [MemoryRegion];
    fn kernel_image_info(&self) -> (VirtAddr, usize);
    fn get_system_table(&self, table: BootInfoSystemTable) -> VirtAddr;
}

#[thread_local]
static FOO: u32 = 1234;
//entry_point!(kernel_main);
fn kernel_main<B: BootInfo>(boot_info: &mut B) -> ! {
    let kernel_image_reg = 0xffffffff80000000u64;
    let clone_regions = [VirtAddr::new(kernel_image_reg)];
    logln!("early memory init");
    memory::init(boot_info, &clone_regions);

    arch::init(boot_info);
    // memory::allocator::init_heap(&mut mapper, &mut frame_allocator)
    // .expect("failed to initialize heap");

    logln!("parsing debug image");
    let (kernel_image_start, kernel_image_length) = boot_info.kernel_image_info();
    unsafe {
        let kernel_image =
            core::slice::from_raw_parts(kernel_image_start.as_ptr(), kernel_image_length);
        image::init(kernel_image);
        panic::init(kernel_image);
    }

    logln!("donel");
    arch::init_interrupts();

    logln!("enumerate CPUS");
    arch::processor::enumerate_cpus();
    processor::init_cpu(image::get_tls());
    arch::init_secondary();

    processor::boot_all_secondaries(image::get_tls());
    logln!("done");

    clock::init();
    let lock = spinlock::Spinlock::<u32>::new(0);
    let mut v = lock.lock();
    *v = 2;

    init_threading();
}

pub fn init_threading() -> ! {
    logln!("got here");
    //arch::schedule_oneshot_tick(1000000000);
    //loop {}
    sched::create_idle_thread();
    clock::schedule_oneshot_tick(1);
    thread::start_new(thread_main);
    thread::start_new(thread_main);
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
        "ahah: mov rax, 1234",
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
