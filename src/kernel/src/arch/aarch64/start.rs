use core::arch::asm;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // let's set the stack
    unsafe { asm!(
        "ldr x30, =__stack_top",
        "mov sp, x30"
    );}

    crate::arch::kernel_main();
}