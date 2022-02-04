//#![no_std]
#![feature(lang_items)]
#![feature(asm)]
#![feature(naked_functions)]
#![feature(thread_local)]
//#![no_main]

/*
#[no_mangle]
pub extern "C" fn std_runtime_starta() {
    twizzler_abi::syscall::sys_kernel_console_write(
        b"hello world\n",
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
    );
    loop {}
}
*/

/*
#[panic_handler]
pub fn __panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
*/

#[thread_local]
static mut FOO: u32 = 42;
#[thread_local]
static mut BAR: u32 = 0;
#[allow(named_asm_labels)]

static BAZ: AtomicU64 = AtomicU64::new(0);

fn test_thread_sync() {
    let j = std::thread::spawn(|| {
        let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
        let value = 0;
        let wait = ThreadSync::new_sleep(ThreadSyncSleep::new(
            reference,
            value,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));

        loop {
            println!("{:?} going to sleep", std::thread::current().id());
            let res = sys_thread_sync(&mut [wait], None);
            println!("woke up: {:?} {:?}", res, wait.get_result());
        }
    });

    let reference = ThreadSyncReference::Virtual(&BAZ as *const AtomicU64);
    let wake = ThreadSync::new_wake(ThreadSyncWake::new(reference, 1));
    loop {
        println!("{:?} waking up", std::thread::current().id());
        let res = sys_thread_sync(&mut [wake], None);
        println!("done {:?}", res);
    }
}

fn main() {
    let _foo = unsafe { FOO + BAR };
    println!("Hello, World {}", unsafe { FOO + BAR });

    test_thread_sync();
    let j = std::thread::spawn(|| {
        for i in 0..1 {
            println!("hello from thread");
        }
        234
    });

    println!("joining...");
    let res = j.join();
    println!("joined {:?}", res);
    //panic!("panic test");
    loop {}
}

/*
#[naked]
#[no_mangle]
extern "C" fn _start() -> ! {
    unsafe { asm!("call std_runtime_start", options(noreturn)) }
}
*/

use std::sync::atomic::AtomicU64;

use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep,
    ThreadSyncWake,
};
