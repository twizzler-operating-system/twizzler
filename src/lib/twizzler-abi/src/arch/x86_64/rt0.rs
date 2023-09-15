#[no_mangle]
#[naked]
pub unsafe extern "C" fn _start() {
    core::arch::asm!(
        "and rsp, 0xfffffffffffffff0",
        "call {entry}",
        "ud2",
        entry = sym trampoline,
        options(noreturn)
    );
}

unsafe extern "C" fn trampoline(arg: usize) {
    twizzler_runtime_api::call_into_runtime_from_rt0(arg);
}

#[used]
static ENTRY: unsafe extern "C" fn() = _start;
