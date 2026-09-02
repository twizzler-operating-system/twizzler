use core::mem::MaybeUninit;

use num_enum::{FromPrimitive, IntoPrimitive};
use twizzler_rt_abi::error::TwzError;

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{
    arch::{syscall::raw_syscall, ArchRegisters},
    object::ObjID,
    thread::ExecutionState,
    upcall::{ResumeFlags, UpcallFrame, UpcallTarget},
};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, FromPrimitive, IntoPrimitive)]
#[repr(u64)]
/// Possible Thread Control operations
pub enum ThreadControl {
    #[default]
    /// Exit the thread. arg1 and arg2 should be code and location respectively, where code
    /// contains a 64-bit value to write into *location, followed by the kernel performing a
    /// thread-wake event on the memory word at location. If location is null, the write and
    /// thread-wake do not occur.
    Exit = 0,
    /// Yield the thread's CPU time now. The actual effect of this is unspecified, but it acts as a
    /// hint to the kernel that this thread does not need to run right now. The kernel, of course,
    /// is free to ignore this hint.
    Yield = 1,
    /// Set thread's TLS pointer
    SetTls = 2,
    /// Get the thread's TLS pointer.
    GetTls = 3,
    /// Set the thread's upcall pointer (child threads in the same virtual address space will
    /// inherit).
    SetUpcall = 4,
    /// Get the upcall pointer.
    GetUpcall = 5,
    /// Read the thread's CPU state. The thread must be suspended.
    ReadRegisters = 6,
    /// Write the thread's CPU state. The thread must be suspended.
    WriteRegisters = 7,
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
    /// Resume from an upcall.
    ResumeFromUpcall = 16,
    /// Get the repr ID of the calling thread.
    GetSelfId = 17,
    /// Get the ID of the active security context.
    GetActiveSctxId = 18,
    /// Set the ID of the active security context.
    SetActiveSctxId = 19,
    /// Set trace events.
    SetTraceEvents = 20,
    /// Get trace events.
    GetTraceEvents = 21,
    /// Read stats
    GetStats = 22,
    /// Read the target's home and active security context ids.
    GetSctxIds = 23,
}

/// Exit the thread. The code will be written to the [crate::thread::ThreadRepr] for the current
/// thread as part of updating the status and code to indicate thread has exited.
pub fn sys_thread_exit(code: u64) -> ! {
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[0, 0, ThreadControl::Exit as u64, code],
        );
    }
    unreachable!()
}

/// Yield the thread's CPU time now. The actual effect of this is unspecified, but it acts as a
/// hint to the kernel that this thread does not need to run right now. The kernel, of course,
/// is free to ignore this hint.
pub fn sys_thread_yield() {
    unsafe {
        raw_syscall(Syscall::ThreadCtrl, &[0, 0, ThreadControl::Yield as u64]);
    }
}

/// Set the current kernel thread's TLS pointer. On x86_64, for example, this changes user's FS
/// segment base to the supplies TLS value.
pub fn sys_thread_settls(tls: u64) {
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[0, 0, ThreadControl::SetTls as u64, tls],
        );
    }
}

/// Get the repr ID of the calling thread.
pub fn sys_thread_self_id() -> ObjID {
    let (hi, lo) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[0, 0, ThreadControl::GetSelfId as u64],
        )
    };
    ObjID::from_parts([hi, lo])
}

/// Get the active security context ID for the calling thread.
pub fn sys_thread_active_sctx_id() -> ObjID {
    let (hi, lo) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[0, 0, ThreadControl::GetActiveSctxId as u64],
        )
    };
    ObjID::from_parts([hi, lo])
}

bitflags::bitflags! {
    /// Options for [sys_thread_set_active_sctx_id_ext].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SctxSwitchFlags: u64 {
        /// Attach the calling thread to the target context if it is not attached already, instead
        /// of failing. Saves a separate [super::sys_sctx_attach] call on the gate-entry path.
        const ATTACH = 1;
    }
}

/// Set the active security context for the calling thread, returning the thread pointer the kernel
/// installed for it.
///
/// The thread pointer is tracked per (thread, context) by the kernel, so switching contexts also
/// switches TLS: the returned value is this thread's pointer in `id`, and is zero the first time
/// this thread runs there. That is the only way for a cross-compartment entry to learn whether it
/// may touch thread-local storage -- from userspace, the read that would test the thread pointer
/// for zero is itself the fault.
pub fn sys_thread_set_active_sctx_id_ext(
    id: ObjID,
    flags: SctxSwitchFlags,
) -> Result<u64, TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                0,
                0,
                ThreadControl::SetActiveSctxId as u64,
                id.parts()[0],
                id.parts()[1],
                flags.bits(),
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v, twzerr)
}

/// Set the active security context for the calling thread.
pub fn sys_thread_set_active_sctx_id(id: ObjID) -> Result<(), TwzError> {
    sys_thread_set_active_sctx_id_ext(id, SctxSwitchFlags::empty()).map(|_| ())
}

/// Get the upcall location for this thread.
pub fn sys_thread_get_upcall() -> Result<UpcallTarget, TwzError> {
    let mut target = MaybeUninit::<UpcallTarget>::uninit();
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                0,
                0,
                ThreadControl::GetUpcall as u64,
                target.as_mut_ptr() as usize as u64,
            ],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        |_, _| unsafe { target.assume_init() },
        twzerr,
    )
}

/// Set the upcall location for this thread.
pub fn sys_thread_set_upcall(target: UpcallTarget) -> Result<(), TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                0,
                0,
                ThreadControl::SetUpcall as u64,
                (&target as *const _) as usize as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}

/// Resume from an upcall, restoring registers. If you can
/// resume yourself in userspace, this call is not necessary.
///
/// # Safety
/// The frame argument must point to a valid upcall frame with
/// a valid register state.
pub unsafe fn sys_thread_resume_from_upcall(frame: &UpcallFrame, flags: ResumeFlags) -> ! {
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                0,
                0,
                ThreadControl::ResumeFromUpcall as u64,
                frame as *const _ as usize as u64,
                flags.bits(),
            ],
        );
        unreachable!()
    }
}

/// Get the current kernel thread's TLS pointer.
pub fn sys_thread_gettls() -> u64 {
    let (tls, _) =
        unsafe { raw_syscall(Syscall::ThreadCtrl, &[0, 0, ThreadControl::GetTls as u64]) };
    tls
}

/// Read the thread's CPU state. The thread must be suspended.
pub fn sys_thread_read_registers(target: ObjID) -> Result<ArchRegisters, TwzError> {
    let mut regs = MaybeUninit::zeroed();
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::ReadRegisters as u64,
                regs.as_mut_ptr() as usize as u64,
            ],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        move |_, _| unsafe { regs.assume_init() },
        twzerr,
    )
}

/// Write the thread's CPU state. The thread must be suspended.
pub fn sys_thread_write_registers(target: ObjID, regs: &ArchRegisters) -> Result<(), TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::WriteRegisters as u64,
                regs as *const _ as usize as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}

/// Send a user-defined async or sync event to the thread.
///
/// The target's mailbox is a bitmask, so `message` names a bit and must be in `1..64`; anything
/// else is an invalid-argument error. Pending messages coalesce per bit and are delivered one per
/// upcall, lowest first, once the thread returns to user in its own security context.
pub fn sys_thread_send_message(target: ObjID, message: u64, flags: u64) -> Result<(), TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::SendMessage as u64,
                message,
                flags,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}

/// Change the thread's state. If successful, returns the previous state.
///
/// A transition to [ExecutionState::Exited] is a force-exit, and it is asynchronous: the target
/// notices it at its next poll point, which may be inside a cross-compartment call, holding that
/// compartment's locks -- and dying there would leave them held forever, wedging that compartment
/// for everyone. The kernel therefore defers delivery until the target is executing in its home
/// security context, the one stamped at spawn from [ThreadSpawnArgs::home_sctx]. A zero home
/// (kernel-spawned and statically-linked threads) means unrestricted delivery.
///
/// [ThreadSpawnArgs::home_sctx]: super::ThreadSpawnArgs
pub fn sys_thread_change_state(
    target: ObjID,
    new_state: ExecutionState,
) -> Result<ExecutionState, TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::ChangeState as u64,
                new_state.to_status(),
            ],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        |_, v| ExecutionState::from_status(v),
        twzerr,
    )
}

/// Set the Trap State for the thread.
pub fn sys_thread_set_trap_state(target: ObjID, trap_state: u64) -> Result<(), TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::SetTrapState as u64,
                trap_state,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}

/// Get the Trap State for the thread.
pub fn sys_thread_get_trap_state(target: ObjID) -> Result<u64, TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::GetTrapState as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v, twzerr)
}

/// Set the Trap State for the thread.
pub fn sys_thread_set_trace_events(target: ObjID, events: u64) -> Result<(), TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::SetTraceEvents as u64,
                events,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}

#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct ThreadSchedStats {
    pub user: u64,
    pub system: u64,
    pub idle: u64,
    /// Page faults charged to this thread since it started. Cumulative, like the three above;
    /// a rate is the reader's job.
    pub faults: u64,
    /// Object pages this thread asked the pager for. Counts the ask, not the arrival: a page
    /// another thread's request already covered is charged to whoever asked for it.
    pub pager_pages: u64,
    /// Syscalls this thread made.
    pub syscalls: u64,
    /// Times this thread went from blocked to runnable. A thread with a high wake rate and
    /// little cpu time is polling: it is being woken to do nothing.
    pub wakes: u64,
}

pub fn sys_thread_read_stats(target: ObjID, stats: &mut ThreadSchedStats) -> Result<(), TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::GetStats as u64,
                stats as *mut _ as usize as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}

/// A thread's two security context ids.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ThreadSctxIds {
    /// The context the thread belongs to, stamped once at spawn from
    /// [ThreadSpawnArgs::home_sctx]. Zero for kernel-spawned and statically-linked threads, and
    /// for the monitor's own threads, none of which have a compartment to belong to.
    ///
    /// [ThreadSpawnArgs::home_sctx]: super::ThreadSpawnArgs
    pub home: ObjID,
    /// The context the thread is executing in right now. Differs from `home` exactly while the
    /// thread is inside a cross-compartment (gate) call, running someone else's code.
    pub active: ObjID,
}

impl ThreadSctxIds {
    /// Whether the thread is currently executing outside its home context, i.e. in a gate call.
    /// Always false for a zero home, which belongs to no compartment and so cannot leave one.
    pub fn is_cross(&self) -> bool {
        self.home.raw() != 0 && self.home != self.active
    }
}

/// Read a thread's home and active security context ids.
///
/// Unlike [sys_thread_active_sctx_id], which only ever reports the caller's own active context,
/// this reads a target thread -- the pair is only meaningful from outside, since a thread
/// observing itself is by construction at home.
pub fn sys_thread_read_sctx_ids(target: ObjID, ids: &mut ThreadSctxIds) -> Result<(), TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::GetSctxIds as u64,
                ids as *mut _ as usize as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}

/// Get the Trap State for the thread.
pub fn sys_thread_get_trace_events(target: ObjID) -> Result<u64, TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::GetTraceEvents as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v, twzerr)
}

/// The scheduling class of a thread, ordered from lowest to highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, FromPrimitive, IntoPrimitive)]
#[repr(u16)]
pub enum PriorityClass {
    Idle = 0,
    Background = 1,
    #[default]
    User = 2,
    Realtime = 3,
}

/// Priority values within a class range over `0..MAX_PRIORITY_VALUE`.
pub const MAX_PRIORITY_VALUE: u16 = 128;

/// A thread's scheduling priority: a class, and a value within that class. A higher class always
/// outranks a lower one, regardless of value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub struct ThreadPriority {
    pub class: PriorityClass,
    pub value: u16,
}

impl ThreadPriority {
    /// The default priority of a userspace thread.
    pub const USER: Self = Self {
        class: PriorityClass::User,
        value: MAX_PRIORITY_VALUE / 2,
    };

    pub const fn new(class: PriorityClass, value: u16) -> Self {
        Self {
            class,
            value: if value < MAX_PRIORITY_VALUE {
                value
            } else {
                MAX_PRIORITY_VALUE - 1
            },
        }
    }

    /// The packed representation exchanged with the kernel.
    pub fn raw(&self) -> u32 {
        ((self.class as u32) << 16) | (self.value as u32)
    }

    pub fn from_raw(raw: u32) -> Self {
        Self::new(
            PriorityClass::from_primitive((raw >> 16) as u16),
            (raw & 0xffff) as u16,
        )
    }
}

/// Set a thread's priority.
pub fn sys_thread_set_priority(target: ObjID, priority: ThreadPriority) -> Result<(), TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::SetPriority as u64,
                priority.raw() as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}

/// Get a thread's priority. This is the thread's base priority -- any priority it has been
/// temporarily donated by the kernel is not reflected here.
pub fn sys_thread_get_priority(target: ObjID) -> Result<ThreadPriority, TwzError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                target.parts()[0],
                target.parts()[1],
                ThreadControl::GetPriority as u64,
            ],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        |_, v| ThreadPriority::from_raw(v as u32),
        twzerr,
    )
}

pub const PERTHREAD_TRACE_GEN_SAMPLE: u64 = 1;

pub fn sys_thread_ctrl(
    target: Option<ObjID>,
    cmd: ThreadControl,
    arg0: usize,
    arg1: usize,
    arg2: usize,
) -> (u64, u64) {
    let target = target.unwrap_or(ObjID::new(0));
    let ids = target.parts();
    unsafe {
        raw_syscall(
            Syscall::ThreadCtrl,
            &[
                ids[0],
                ids[1],
                cmd as u64,
                arg0 as u64,
                arg1 as u64,
                arg2 as u64,
            ],
        )
    };
    todo!("not ready yet!")
}
