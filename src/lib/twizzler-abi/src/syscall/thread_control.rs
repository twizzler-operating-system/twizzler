use crate::{arch::syscall::raw_syscall, upcall::{UpcallFrame, UpcallInfo}};

use super::Syscall;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u64)]
/// Possible Thread Control operations
pub enum ThreadControl {
    /// Exit the thread. arg1 and arg2 should be code and location respectively, where code contains
    /// a 64-bit value to write into *location, followed by the kernel performing a thread-wake
    /// event on the memory word at location. If location is null, the write and thread-wake do not occur.
    Exit = 0,
    /// Yield the thread's CPU time now. The actual effect of this is unspecified, but it acts as a
    /// hint to the kernel that this thread does not need to run right now. The kernel, of course,
    /// is free to ignore this hint.
    Yield = 1,
    /// Set thread's TLS pointer
    SetTls = 2,
    /// Set the thread's upcall pointer (child threads in the same virtual address space will inherit).
    SetUpcall = 3,
}

impl From<u64> for ThreadControl {
    fn from(x: u64) -> Self {
        match x {
            0 => Self::Exit,
            1 => Self::Yield,
            2 => Self::SetTls,
            3 => Self::SetUpcall,
            _ => Self::Yield,
        }
    }
}

/// Exit the thread. arg1 and arg2 should be code and location respectively, where code contains
/// a 64-bit value to write into *location, followed by the kernel performing a thread-wake
/// event on the memory word at location. If location is null, the write and thread-wake do not occur.
pub fn sys_thread_exit(code: u64, location: *mut u64) -> ! {
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[ThreadControl::Exit as u64, code, location as u64],
        );
    }
    unreachable!()
}

/// Yield the thread's CPU time now. The actual effect of this is unspecified, but it acts as a
/// hint to the kernel that this thread does not need to run right now. The kernel, of course,
/// is free to ignore this hint.
pub fn sys_thread_yield() {
    unsafe {
        raw_syscall(Syscall::ThreadCtrl, &[ThreadControl::Yield as u64]);
    }
}

/// Set the current kernel thread's TLS pointer. On x86_64, for example, this changes user's FS
/// segment base to the supplies TLS value.
pub fn sys_thread_settls(tls: u64) {
    unsafe {
        raw_syscall(Syscall::ThreadCtrl, &[ThreadControl::SetTls as u64, tls]);
    }
}

pub fn sys_thread_set_upcall(
    loc: unsafe extern "C" fn(*const UpcallFrame, *const UpcallInfo) -> !,
) {
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[ThreadControl::SetUpcall as u64, loc as usize as u64],
        );
    }
}
