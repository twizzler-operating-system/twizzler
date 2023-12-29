fn foo_impl() -> u32 {
    42
}

pub fn do_setup() {}

pub fn do_teardown() {}

type FooEntryType = extern "C" fn() -> SecGateReturn<u32>;

#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub enum SecGateReturn<T> {
    Success(T),
    PermissionDenied,
    CalleePanic,
}

use std::process::{ExitCode, Termination};
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

#[repr(C)]
pub struct SecGateInfo<F> {
    imp: F,
}

#[link_section = ".twz_secgate_info"]
#[used]
static FOO_INFO: SecGateInfo<&'static FooEntryType> = SecGateInfo {
    imp: &(foo_entry as FooEntryType),
};

#[link_section = ".twz_secgate_text"]
#[naked]
pub unsafe extern "C" fn foo_trampoline() -> SecGateReturn<u32> {
    core::arch::asm!("jmp {}", sym foo_entry, options(noreturn))
}

pub const SECGATE_TRAMPOLINE_ALIGN: usize = 0x10;

#[inline(always)]
pub fn foo() -> SecGateReturn<u32> {
    unsafe { foo_trampoline() }
}
