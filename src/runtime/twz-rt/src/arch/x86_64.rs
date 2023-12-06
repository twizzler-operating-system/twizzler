use twizzler_abi::upcall::{UpcallData, UpcallFrame};

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn rr_upcall_entry(
    rdi: *mut UpcallFrame,
    rsi: *const UpcallData,
) -> ! {
    core::arch::asm!(
        "and rsp, 0xfffffffffffffff0",
        "mov rbp, rdx",
        "push rax",
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
pub(crate) unsafe extern "C-unwind" fn rr_upcall_entry2(
    rdi: *mut UpcallFrame,
    rsi: *const UpcallData,
) -> ! {
    use twizzler_abi::{syscall::sys_thread_exit, upcall::UPCALL_EXIT_CODE};

    let handler = || crate::runtime::upcall::upcall_rust_entry(&mut *rdi, &*rsi);

    if std::panic::catch_unwind(handler).is_err() {
        sys_thread_exit(UPCALL_EXIT_CODE);
    }
    // TODO: with uiret instruction, we may be able to avoid the kernel, here.
    twizzler_abi::syscall::sys_thread_resume_from_upcall(&*rdi);
}
