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
#[unsafe(naked)]
/// TLS descriptor resolver for static TLS relocations
pub unsafe extern "C" fn _tlsdesc_static(desc: *const TlsDesc) {
    // The offset for the variable in the static TLS block is
    // simply the second word from the TLS descriptor.
    // The result is returned in x0.
    core::arch::naked_asm!("ldr x0, [x0, #8]", "ret");
}

/// TLS descriptor resolver for runtime-loaded modules: the descriptor's value is a `tls_index`.
///
/// Returns the TP-relative offset in x0 and preserves every other register, as the descriptor
/// ABI requires. Fast path: the module's block address from this thread's DTV (`Tcb` sits 128
/// bytes below TP: `self_ptr, dtv_len, dtv`). Slow path: `__tls_get_addr`, which grows the DTV
/// for a module loaded after this thread's region was built.
#[no_mangle]
#[unsafe(naked)]
pub unsafe extern "C" fn _tlsdesc_dynamic(desc: *const TlsDesc) {
    core::arch::naked_asm!(
        "stp x1, x2, [sp, #-48]!",
        "stp x3, x4, [sp, #16]",
        "mrs x4, nzcv",
        "str x4, [sp, #32]",
        "ldr x1, [x0, #8]",
        "mrs x4, tpidr_el0",
        "sub x2, x4, #128",
        "ldr x3, [x1]",
        "ldr x0, [x2, #8]",
        "cmp x3, x0",
        "b.hs 2f",
        "ldr x0, [x2, #16]",
        "ldr x0, [x0, x3, lsl #3]",
        "cbz x0, 2f",
        "ldr x3, [x1, #8]",
        "add x0, x0, x3",
        "sub x0, x0, x4",
        "1:",
        "ldr x4, [sp, #32]",
        "msr nzcv, x4",
        "ldp x3, x4, [sp, #16]",
        "ldp x1, x2, [sp], #48",
        "ret",
        // Slow path: a real call, so save what it may clobber (x5-x18, x30, v0-v31).
        "2:",
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "sub sp, sp, #640",
        "stp x5, x6, [sp, #0]",
        "stp x7, x8, [sp, #16]",
        "stp x9, x10, [sp, #32]",
        "stp x11, x12, [sp, #48]",
        "stp x13, x14, [sp, #64]",
        "stp x15, x16, [sp, #80]",
        "stp x17, x18, [sp, #96]",
        "stp q0, q1, [sp, #128]",
        "stp q2, q3, [sp, #160]",
        "stp q4, q5, [sp, #192]",
        "stp q6, q7, [sp, #224]",
        "stp q8, q9, [sp, #256]",
        "stp q10, q11, [sp, #288]",
        "stp q12, q13, [sp, #320]",
        "stp q14, q15, [sp, #352]",
        "stp q16, q17, [sp, #384]",
        "stp q18, q19, [sp, #416]",
        "stp q20, q21, [sp, #448]",
        "stp q22, q23, [sp, #480]",
        "stp q24, q25, [sp, #512]",
        "stp q26, q27, [sp, #544]",
        "stp q28, q29, [sp, #576]",
        "stp q30, q31, [sp, #608]",
        "mov x0, x1",
        "bl {get_addr}",
        "cbz x0, 3f",
        "mrs x1, tpidr_el0",
        "sub x0, x0, x1",
        "ldp q30, q31, [sp, #608]",
        "ldp q28, q29, [sp, #576]",
        "ldp q26, q27, [sp, #544]",
        "ldp q24, q25, [sp, #512]",
        "ldp q22, q23, [sp, #480]",
        "ldp q20, q21, [sp, #448]",
        "ldp q18, q19, [sp, #416]",
        "ldp q16, q17, [sp, #384]",
        "ldp q14, q15, [sp, #352]",
        "ldp q12, q13, [sp, #320]",
        "ldp q10, q11, [sp, #288]",
        "ldp q8, q9, [sp, #256]",
        "ldp q6, q7, [sp, #224]",
        "ldp q4, q5, [sp, #192]",
        "ldp q2, q3, [sp, #160]",
        "ldp q0, q1, [sp, #128]",
        "ldp x17, x18, [sp, #96]",
        "ldp x15, x16, [sp, #80]",
        "ldp x13, x14, [sp, #64]",
        "ldp x11, x12, [sp, #48]",
        "ldp x9, x10, [sp, #32]",
        "ldp x7, x8, [sp, #16]",
        "ldp x5, x6, [sp, #0]",
        "mov sp, x29",
        "ldp x29, x30, [sp], #16",
        "b 1b",
        "3:",
        "brk #0",
        get_addr = sym crate::syms::__tls_get_addr,
    );
}
