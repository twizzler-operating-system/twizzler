#[allow(unused_imports)]
use crate::upcall::UpcallInfo;

#[repr(C)]
pub struct UpcallFrame {
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
}

impl UpcallFrame {
    pub fn ip(&self) -> usize {
        self.rip as usize
    }

    pub fn sp(&self) -> usize {
        self.rsp as usize
    }

    pub fn bp(&self) -> usize {
        self.rbp as usize
    }
}

#[cfg(feature = "rt")]
#[no_mangle]
pub(crate) unsafe extern "C" fn upcall_entry(rdi: *const UpcallFrame, rsi: *const UpcallInfo) -> ! {
    crate::upcall::upcall_rust_entry(&*rdi, &*rsi);

    // TODO: resume
    crate::syscall::sys_thread_exit(129, core::ptr::null_mut());
}
