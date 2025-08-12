use core::sync::atomic::AtomicU64;

use twizzler_rt_abi::object::ObjID;

use crate::{
    arch::ArchRegisters,
    pager::{CompletionToKernel, CompletionToPager, KernelCommand, PagerRequest},
    syscall::{MapFlags, TimeSpan},
    upcall::UpcallFrame,
};

#[repr(C)]
pub struct TraceEntryHead {
    thread: ObjID,
    sctx: ObjID,
    mctx: ObjID,
    cpuid: u64,
    time: TimeSpan,
    event: u64,
    kind: TraceKind,
    flags: TraceEntryFlags,
}

#[repr(C)]
pub struct TraceData<T> {
    len: u32,
    data: T,
}

#[repr(C)]
pub struct TraceBase {
    end: AtomicU64,
}

#[repr(u16)]
pub enum TraceKind {
    Kernel,
    Thread,
    Object,
    Context,
    Security,
    Pager,
    Other = 0xffff,
}

bitflags::bitflags! {
    pub struct TraceFlags: u16 {
        const DATA = 1;
        const REGISTERS = 2;
    }
}

bitflags::bitflags! {
    pub struct TraceEntryFlags: u16 {
        const DROPPED = 1;
        const HAS_DATA = 2;
    }
}

pub const THREAD_EXIT: u64 = 1;
pub const THREAD_CONTEXT_SWITCH: u64 = 2;
pub const THREAD_SAMPLE: u64 = 4;
pub const THREAD_SYSCALL_ENTRY: u64 = 8;
pub const THREAD_BLOCK: u64 = 0x10;
pub const THREAD_RESUME: u64 = 0x20;

pub const OBJECT_CTRL: u64 = 1;
pub const OBJECT_CREATE: u64 = 2;

pub const CONTEXT_MAP: u64 = 1;
pub const CONTEXT_UNMAP: u64 = 2;
pub const CONTEXT_FAULT: u64 = 4;

pub const SECURITY_CTX_ENTRY: u64 = 1;
pub const SECURITY_CTX_EXIT: u64 = 2;
pub const SECURITY_VIOLATION: u64 = 4;

pub const KERNEL_ALLOC: u64 = 1;

pub const PAGER_COMMAND_SEND: u64 = 1;
pub const PAGER_COMMAND_RESPONDED: u64 = 2;
pub const PAGER_REQUEST_RECV: u64 = 4;
pub const PAGER_REQUEST_COMPLETED: u64 = 8;

#[repr(C)]
pub struct ThreadEvent {
    val: u64,
    regs: Option<ArchRegisters>,
}

#[repr(C)]
pub struct ThreadCtxSwitch {
    to: Option<ObjID>,
    regs: Option<ArchRegisters>,
}

#[repr(C)]
pub struct ContextMapEvent {
    addr: u64,
    len: u64,
    obj: ObjID,
    flags: MapFlags,
    regs: Option<ArchRegisters>,
}

bitflags::bitflags! {
    pub struct FaultFlags: u64 {
        const READ = 1;
        const WRITE = 2;
        const EXEC = 4;
        const USER = 8;
        const PAGER = 0x10;
    }
}

pub struct ContextFaultEvent {
    addr: u64,
    obj: ObjID,
    flags: FaultFlags,
    regs: Option<ArchRegisters>,
}

pub struct PagerCommandSent {
    pub cmd: KernelCommand,
    pub qid: u32,
    pub regs: Option<ArchRegisters>,
}

pub struct PagerCommandResponded {
    pub qid: u32,
    pub resp: CompletionToKernel,
    pub regs: Option<ArchRegisters>,
}

pub struct PagerRequestRecv {
    pub req: PagerRequest,
    pub qid: u32,
    pub regs: Option<ArchRegisters>,
}

pub struct PagerRequestCompleted {
    pub qid: u32,
    pub resp: CompletionToPager,
    pub regs: Option<ArchRegisters>,
}
