use twizzler_abi::upcall::{ResumeFlags, UpcallData, UpcallFrame};
use twizzler_rt_abi::thread::TlsDesc;

#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn twz_rt_upcall_entry_c(
    frame: *mut UpcallFrame,
    info: *const UpcallData,
) -> ! {
    use twizzler_abi::{syscall::sys_thread_exit, upcall::UPCALL_EXIT_CODE};

    let handler = || crate::runtime::upcall::upcall_rust_entry(&mut *frame, &*info);

    if std::panic::catch_unwind(handler).is_err() {
        sys_thread_exit(UPCALL_EXIT_CODE);
    }
    twizzler_abi::syscall::sys_thread_resume_from_upcall(&*frame, ResumeFlags::empty());
}

#[no_mangle]
#[naked]
/// TLS descriptor resolver for static TLS relocations
pub unsafe extern "C" fn _tlsdesc_static(desc: *const TlsDesc) {
    // The offset for the variable in the static TLS block is
    // simply the second word from the TLS descriptor.
    // The result is returned in x0.
    core::arch::naked_asm!("ldr x0, [x0, #8]", "ret");
}
