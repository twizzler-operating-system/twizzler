use num_enum::{FromPrimitive, IntoPrimitive};

use crate::{
    arch::syscall::raw_syscall,
    object::ObjID,
    upcall::{UpcallFrame, UpcallInfo, UpcallTarget},
};

use super::Syscall;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, FromPrimitive, IntoPrimitive)]
#[repr(u64)]
/// Possible Thread Control operations
pub enum ThreadControl {
    #[default]
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
    /// Get the thread's TLS pointer.
    GetTls = 3,
    /// Set the thread's upcall pointer (child threads in the same virtual address space will inherit).
    SetUpcall = 4,
    /// Get the upcall pointer.
    GetUpcall = 5,
    /// Read a register from the thread's CPU state. The thread must be suspended.
    ReadRegister = 6,
    /// Write a value to a register in the thread's CPU state. The thread must be suspended.
    WriteRegister = 7,
    /// Send a user-defined async or sync event to the thread.
    SendMessage = 8,
    /// Change the thread's state. Allowed transitions are:
    /// running -> suspended
    /// suspended -> running
    /// running -> exited
    ChangeState = 9,
    /// Set the Trap State for the thread.
    SetTrapState = 10,
    /// Get the Trap State for the thread.
    GetTrapState = 11,
    /// Set a thread's priority. Threads require special permission to increase their priority.
    SetPriority = 12,
    /// Get a thread's priority.
    GetPriority = 13,
    /// Set a thread's affinity.
    SetAffinity = 14,
    /// Get a thread's affinity.
    GetAffinity = 15,
}

/// Exit the thread. The code will be written to the [crate::thread::ThreadRepr] for the current thread as part
/// of updating the status and code to indicate thread has exited.
pub fn sys_thread_exit(code: u64) -> ! {
    unsafe {
        raw_syscall(Syscall::ThreadCtrl, &[ThreadControl::Exit as u64, code]);
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

/// Set the upcall location for this thread.
pub fn sys_thread_set_upcall(target: UpcallTarget) {
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                ThreadControl::SetUpcall as u64,
                (&target as *const _) as usize as u64,
            ],
        );
    }
}

pub fn sys_thread_ctrl(
    target: Option<ObjID>,
    cmd: ThreadControl,
    arg0: usize,
    arg1: usize,
    arg2: usize,
) -> (u64, u64) {
    let target = target.unwrap_or(ObjID::new(0));
    let ids = target.split();
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                ids.0,
                ids.1,
                cmd as u64,
                arg0 as u64,
                arg1 as u64,
                arg2 as u64,
            ],
        )
    };
    todo!("not ready yet!")
}
