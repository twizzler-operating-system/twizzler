/*
pub fn do_setup() {}

pub fn do_teardown() {}

use std::process::{ExitCode, Termination};

use secgate::{SecGateInfo, SecGateReturn};

type FooEntryType = extern "C" fn() -> SecGateReturn<u32>;
fn foo_impl() -> u32 {
    42
}

pub extern "C" fn foo_entry() -> SecGateReturn<u32> {
    do_setup();

    let ret = std::panic::catch_unwind(|| foo_impl());
    if ret.is_err() {
        Termination::report(ExitCode::from(101u8));
    }
    do_teardown();
    match ret {
        Ok(r) => SecGateReturn::Success(r),
        Err(_) => SecGateReturn::CalleePanic,
    }
}

#[link_section = ".twz_secgate_info"]
#[used]
static FOO_INFO: SecGateInfo<&'static FooEntryType> =
    SecGateInfo::new(&(foo_entry as FooEntryType), c"foo");

#[link_section = ".twz_secgate_text"]
#[naked]
pub unsafe extern "C" fn foo_trampoline() -> SecGateReturn<u32> {
    core::arch::asm!("jmp {}", sym foo_entry, options(noreturn))
}

#[inline(always)]
pub fn foo() -> SecGateReturn<u32> {
    unsafe { foo_trampoline() }
}
*/

#[secgate::secure_gate]
fn bar(x: i32, y: bool) -> u32 {
    tracing::info!("in sec gate bar: {} {}", x, y);
    420
}
