//#![feature(naked_functions)]
#![feature(start)]

use twizzler_runtime_api::AuxEntry;

#[no_mangle]
pub extern "C" fn monitor_entry_from_bootstrap(aux: *const AuxEntry) {
    let _ = twizzler_abi::syscall::sys_kernel_console_write(
        b"hello world from monitor entry\n",
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
    );
    unsafe { twizzler_runtime_api::rt0::rust_entry(aux) }
}

pub fn my_main() {
    let _ = twizzler_abi::syscall::sys_kernel_console_write(
        b"hello world from monitor main\n",
        twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
    );
    loop {}
}

#[allow(improper_ctypes)]
extern "C" {
    fn twizzler_call_lang_start(
        main: fn(),
        argc: isize,
        argv: *const *const u8,
        sigpipe: u8,
    ) -> isize;
}

#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn main(argc: i32, argv: *const *const u8) -> i32 {
    //TODO: sigpipe?
    unsafe { twizzler_call_lang_start(my_main, argc as isize, argv, 0) as i32 }
}

#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn _init() {}

/*
#[secure_gate]
pub fn stream_writer_write() {}

#[link_section = ".twz_gate_data"]
pub static FOO_GATE: SecurityGate<fn(i32, bool) -> Option<bool>, (i32, bool), Option<bool>> =
    SecurityGate::new(foo_gate_impl);

fn foo_gate_impl(x: i32, y: bool) -> Option<bool> {
    if x == 0 {
        Some(!y)
    } else {
        None
    }
}

#[link_section = ".twz_gate_text"]
#[naked]
pub extern "C" fn foo_gate_impl_trampoline(x: i32, y: bool) -> Option<bool> {
    unsafe { core::arch::asm!("jmp foo_gate_trampoline_c_entry", options(noreturn)) }
}

extern "C" fn foo_gate_trampoline_c_entry(x: i32, y: bool) -> Option<bool> {
    // pre-call setup (secure callee side)
    let ret = foo_gate_impl(x, y);
    // post-call tear-down (secure callee side)
    ret
}

pub fn foo(x: i32, y: bool) -> Option<bool> {
    (FOO_GATE)(x, y)
}

*/

/*
#[secure_gate]
fn foo(x: u32, y: bool) -> Option<bool> {
    if x == 0 {
        Some(!y)
    } else {
        None
    }
}
*/
