fn foo_impl() -> u32 {
    42
}

pub fn do_setup() {}

pub fn do_teardown() {}

type FooEntryType = extern "C" fn() -> u32;

pub extern "C" fn foo_entry() -> u32 {
    do_setup();
    let ret = std::panic::catch_unwind(|| foo_impl());
    do_teardown();
    match ret {
        Ok(r) => r,
        Err(_) => todo!(),
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
pub unsafe extern "C" fn foo_trampoline() -> u32 {
    core::arch::asm!("jmp {}", sym foo_entry, options(noreturn))
}

pub const SECGATE_TRAMPOLINE_ALIGN: usize = 0x10;

#[inline(always)]
pub fn foo() -> u32 {
    unsafe { foo_trampoline() }
}
