#[no_mangle]
#[naked]
#[allow(named_asm_labels)]
pub unsafe extern "C" fn _start() {
    asm!(
        "and rsp, 0xfffffffffffffff0",
        "mov rax, 0x1234",
        "ahaha:",
        "jmp ahaha",
        "call {entry}",
        "ud2",
        entry = sym crate::rt1::twz_runtime_start,
        options(noreturn)
    );
}

#[used]
static ENTRY: unsafe extern "C" fn() = _start;
