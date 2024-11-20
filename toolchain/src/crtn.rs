//! crti

#![no_std]
#![allow(internal_features)]
#![feature(linkage)]
#![feature(core_intrinsics)]

// https://wiki.osdev.org/Creating_a_C_Library#crtbegin.o.2C_crtend.o.2C_crti.o.2C_and_crtn.o
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    r#"
    .section .init
        // This happens after crti.o and gcc has inserted code
        // Pop the stack frame
        pop rbp
        ret

    .section .fini
        // This happens after crti.o and gcc has inserted code
        // Pop the stack frame
        pop rbp
        ret
"#
);
// https://git.musl-libc.org/cgit/musl/tree/crt/aarch64/crtn.s
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    r#"
    .section .init
        // This happens after crti.o and gcc has inserted code
        // ldp: "loads two doublewords from memory addressed by the third argument to the first and second"
        ldp x29,x30,[sp],#16
        ret

    .section .fini
        // This happens after crti.o and gcc has inserted code
        // ldp: "loads two doublewords from memory addressed by the third argument to the first and second"
        ldp x29,x30,[sp],#16
        ret
"#
);

#[panic_handler]
#[linkage = "weak"]
#[no_mangle]
pub unsafe fn rust_begin_unwind(_pi: &::core::panic::PanicInfo) -> ! {
    core::intrinsics::abort()
}
