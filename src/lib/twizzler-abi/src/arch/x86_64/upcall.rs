#[allow(unused_imports)]
use crate::upcall::{UpcallData, UpcallInfo};

pub const XSAVE_LEN: usize = 1024;

/// Arch-specific frame info for upcall.
#[derive(Clone, Debug, Copy)]
#[repr(C, align(64))]
pub struct UpcallFrame {
    pub xsave_region: [u8; XSAVE_LEN],
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub thread_ptr: u64,
    pub prior_ctx: crate::object::ObjID,
}

impl UpcallFrame {
    /// Get the instruction pointer of the frame.
    pub fn ip(&self) -> usize {
        self.rip as usize
    }

    /// Get the stack pointer of the frame.
    pub fn sp(&self) -> usize {
        self.rsp as usize
    }

    /// Get the base pointer of the frame.
    pub fn bp(&self) -> usize {
        self.rbp as usize
    }
}

#[no_mangle]
#[cfg(feature = "runtime")]
pub(crate) unsafe extern "C" fn upcall_entry2(
    rdi: *const UpcallFrame,
    rsi: *const UpcallData,
) -> ! {
    use crate::runtime::__twz_get_runtime;

    crate::runtime::upcall::upcall_rust_entry(&*rdi, &*rsi);
    let runtime = __twz_get_runtime();
    runtime.abort()
}

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn upcall_entry(
    rdi: *mut UpcallFrame,
    rsi: *const UpcallData,
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
        "jmp upcall_entry2",
        in("rax") (&*rdi).rip,
        in("rdx") (&*rdi).rbp,
        in("rdi") rdi,
        in("rsi") rsi,
        options(noreturn)
    );
}
