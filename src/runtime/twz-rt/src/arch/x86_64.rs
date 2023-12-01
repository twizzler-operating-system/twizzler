use twizzler_abi::upcall::{UpcallFrame, UpcallInfo};

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C" fn rr_upcall_entry(
    rdi: *const UpcallFrame,
    rsi: *const UpcallInfo,
) -> ! {
    core::arch::asm!(
        ".cfi_signal_frame",
        "mov rbp, rdx",
        "push rax",
        "push rbp",
        "push rax",
        ".cfi_def_cfa rsp, 0",
        ".cfi_offset rbp, 8",
        ".cfi_offset rip, 0",
        ".cfi_return_column rip",
        "jmp rr_upcall_entry2",
        in("rax") (*rdi).rip,
        in("rdx") (*rdi).rbp,
        in("rdi") rdi,
        in("rsi") rsi,
        options(noreturn)
    );
}

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C" fn rr_upcall_entry2(
    rdi: *const UpcallFrame,
    rsi: *const UpcallInfo,
) -> ! {
    crate::runtime::upcall::upcall_rust_entry(&*rdi, &*rsi);
    // TODO: with uiret instruction, we may be able to avoid the kernel, here.
    twizzler_abi::syscall::sys_thread_resume_from_upcall(&*rdi);
}
