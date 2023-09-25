#[no_mangle]
#[naked]
pub unsafe extern "C" fn _start() {
    // Align the stack and jump to rust code. If we come back, trigger an exception.
    core::arch::asm!(
        "and rsp, 0xfffffffffffffff0",
        "call {entry}",
        "ud2",
        entry = sym crate::rt0::entry,
        options(noreturn)
    );
}

#[used]
static ENTRY: unsafe extern "C" fn() = _start;
