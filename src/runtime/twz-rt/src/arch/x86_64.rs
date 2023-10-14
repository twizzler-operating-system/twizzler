use twizzler_abi::upcall::{UpcallFrame, UpcallInfo};

use crate::preinit_println;

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
    use crate::runtime::do_impl::__twz_get_runtime;

    preinit_println!(
        "got upcall: {:?}, {:?}",
        rdi.as_ref().unwrap(),
        rsi.as_ref().unwrap()
    );
    //crate::runtime::upcall::upcall_rust_entry(&*rdi, &*rsi);
    let runtime = __twz_get_runtime();
    runtime.abort()
}
