#[no_mangle]
#[naked]
pub unsafe extern "C" fn _start() {
    core::arch::asm!(
        "and rsp, 0xfffffffffffffff0",
        "call {entry}",
        "ud2",
        entry = sym crate::rt1::twz_runtime_start,
        options(noreturn)
    );
}

#[used]
static ENTRY: unsafe extern "C" fn() = _start;
