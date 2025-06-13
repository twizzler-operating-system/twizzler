#[allow(unused_imports)]
use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallInfo};

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C" fn upcall_entry2(
    rdi: *const UpcallFrame,
    rsi: *const UpcallData,
) -> ! {
    unsafe {
        crate::runtime::upcall::upcall_rust_entry(&*rdi, &*rsi);
    }
    crate::runtime::OUR_RUNTIME.abort()
}

#[unsafe(no_mangle)]
pub(crate) unsafe extern "C-unwind" fn upcall_entry(
    rdi: *mut core::ffi::c_void,
    rsi: *const core::ffi::c_void,
) -> ! {
    let rdi: *const UpcallFrame = rdi.cast();
    unsafe {
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
            "jmp upcall_entry2",
            in("rax") (&*rdi).rip,
            in("rdx") (&*rdi).rbp,
            in("rdi") rdi,
            in("rsi") rsi,
            options(noreturn)
        );
    }
}
