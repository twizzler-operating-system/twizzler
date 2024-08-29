#[allow(unused_imports)]
use crate::upcall::{UpcallData, UpcallInfo};

pub const XSAVE_LEN: usize = 1024;

/// Arch-specific frame info for upcall.
#[derive(Clone, Copy)]
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

impl core::fmt::Debug for UpcallFrame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UpcallFrame")
            .field("rip", &format_args!("0x{:x}", self.rip))
            .field("rsp", &format_args!("0x{:x}", self.rsp))
            .field("rbp", &format_args!("0x{:x}", self.rbp))
            .field("rflags", &format_args!("0x{:x}", self.rflags))
            .field("thread_ptr", &format_args!("0x{:x}", self.thread_ptr))
            .field("prior_ctx", &format_args!("0x{:x}", self.prior_ctx))
            .finish_non_exhaustive()
    }
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

    /// Build a new frame set up to enter a context at a start point.
    pub fn new_entry_frame(
        stack_base: usize,
        stack_size: usize,
        tp: usize,
        ctx: crate::object::ObjID,
        entry: usize,
        arg: usize,
    ) -> Self {
        Self {
            xsave_region: [0; XSAVE_LEN],
            rip: entry as u64,
            // Default has the interrupt enabled flag set, and the reserved bit.
            rflags: 0x202,
            rsp: (stack_base + stack_size - 8) as u64,
            rbp: 0,
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rdi: arg as u64,
            rsi: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            thread_ptr: tp as u64,
            prior_ctx: ctx,
        }
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
