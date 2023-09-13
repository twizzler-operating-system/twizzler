#![feature(naked_functions)]
#![feature(asm_sym)]
use secgate::{secure_gate, SecurityGate};

#[no_mangle]
pub fn monitor_main() {
    println!("Hello, world!");
}

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

#[secure_gate]
fn foo(x: u32, y: bool) -> Option<bool> {
    if x == 0 {
        Some(!y)
    } else {
        None
    }
}
