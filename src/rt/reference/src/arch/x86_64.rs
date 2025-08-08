use twizzler_abi::upcall::{ResumeFlags, UpcallData, UpcallFrame};

#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn twz_rt_upcall_entry_c(
    rdi: *mut UpcallFrame,
    rsi: *const UpcallData,
) -> ! {
    use twizzler_abi::{syscall::sys_thread_exit, upcall::UPCALL_EXIT_CODE};

    let handler = || crate::runtime::upcall::upcall_rust_entry(&mut *rdi, &*rsi);

    if std::panic::catch_unwind(handler).is_err() {
        sys_thread_exit(UPCALL_EXIT_CODE);
    }
    // TODO: with uiret instruction, we may be able to avoid the kernel, here.
    twizzler_abi::syscall::sys_thread_resume_from_upcall(&*rdi, ResumeFlags::empty());
}
