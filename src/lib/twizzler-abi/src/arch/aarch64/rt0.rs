#[no_mangle]
#[naked]
pub unsafe extern "C" fn _start() {
    core::arch::asm!(
        "b {entry}",
        entry = sym crate::rt1::twz_runtime_start,
        options(noreturn)
    );
}

#[used]
static ENTRY: unsafe extern "C" fn() = _start;
