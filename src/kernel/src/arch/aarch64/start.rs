use core::arch::asm;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // let's set some random value in the register
    unsafe { asm!("movz x15, 0xAAAA");}

    // spin in software for now
    loop { }
}