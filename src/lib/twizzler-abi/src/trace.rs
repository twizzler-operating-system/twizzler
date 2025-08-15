use core::sync::atomic::AtomicU64;

use twizzler_rt_abi::object::ObjID;

use crate::{
    pager::{CompletionToKernel, CompletionToPager, KernelCommand, PagerRequest},
    syscall::{
        MapFlags, Syscall, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
        TimeSpan,
    },
};

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct TraceEntryHead {
    pub thread: ObjID,
    pub sctx: ObjID,
    pub mctx: ObjID,
    pub cpuid: u64,
    pub time: TimeSpan,
    pub event: u64,
    pub kind: TraceKind,
    pub extra_or_next: ObjID,
    pub flags: TraceEntryFlags,
}

impl TraceEntryHead {
    pub fn new_next_object(id: ObjID) -> Self {
        Self {
            extra_or_next: id,
            flags: TraceEntryFlags::NEXT_OBJECT,
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct TraceData<T: Copy> {
    pub resv: u64,
    pub len: u32,
    pub flags: u32,
    pub data: T,
}

impl<T: Copy> TraceData<T> {
    pub fn try_cast<U: TraceDataCast + Copy>(&self, events: u64) -> Option<&TraceData<U>> {
        if events & U::EVENT != 0 {
            unsafe {
                Some(
                    (self as *const Self)
                        .cast::<TraceData<U>>()
                        .as_ref()
                        .unwrap(),
                )
            }
        } else {
            None
        }
    }
}

#[repr(C)]
pub struct TraceBase {
    pub end: AtomicU64,
    pub start: u64,
}

impl TraceBase {
    pub fn waiter(&self, pos: u64) -> ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.end),
            pos,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, PartialOrd, Ord)]
#[repr(u16)]
pub enum TraceKind {
    Kernel,
    Thread,
    Object,
    Context,
    Security,
    Pager,
    #[default]
    Other = 0xffff,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct TraceFlags: u16 {
        const DATA = 1;
        //const REGISTERS = 2;
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default)]
    pub struct TraceEntryFlags: u16 {
        const DROPPED = 1;
        const HAS_DATA = 2;
        const NEXT_OBJECT = 4;
    }
}

pub const THREAD_EXIT: u64 = 1;
pub const THREAD_CONTEXT_SWITCH: u64 = 2;
pub const THREAD_SAMPLE: u64 = 4;
pub const THREAD_SYSCALL_ENTRY: u64 = 8;
pub const THREAD_BLOCK: u64 = 0x10;
pub const THREAD_RESUME: u64 = 0x20;
pub const THREAD_MIGRATE: u64 = 0x40;

pub const OBJECT_CTRL: u64 = 1;
pub const OBJECT_CREATE: u64 = 2;

pub const CONTEXT_MAP: u64 = 1;
pub const CONTEXT_UNMAP: u64 = 2;
pub const CONTEXT_FAULT: u64 = 4;
pub const CONTEXT_SHOOTDOWN: u64 = 8;
pub const CONTEXT_INVALIDATION: u64 = 16;

pub const SECURITY_CTX_ENTRY: u64 = 1;
pub const SECURITY_CTX_EXIT: u64 = 2;
pub const SECURITY_VIOLATION: u64 = 4;

pub const KERNEL_ALLOC: u64 = 1;

pub const PAGER_COMMAND_SEND: u64 = 1;
pub const PAGER_COMMAND_RESPONDED: u64 = 2;
pub const PAGER_REQUEST_RECV: u64 = 4;
pub const PAGER_REQUEST_COMPLETED: u64 = 8;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ThreadEvent {
    pub val: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SyscallEntryEvent {
    pub ip: u64,
    pub x: [u64; 4],
    pub num: Syscall,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ThreadCtxSwitch {
    pub to: Option<ObjID>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ThreadMigrate {
    pub to: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ContextMapEvent {
    pub addr: u64,
    pub len: u64,
    pub obj: ObjID,
    pub flags: MapFlags,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    pub struct FaultFlags: u64 {
        const READ = 1;
        const WRITE = 2;
        const EXEC = 4;
        const USER = 8;
        const PAGER = 0x10;
        const LARGE = 0x20;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ContextFaultEvent {
    pub addr: u64,
    pub obj: ObjID,
    pub flags: FaultFlags,
    pub processing_time: TimeSpan,
}

#[derive(Clone, Copy, Debug)]
pub struct PagerCommandSent {
    pub cmd: KernelCommand,
    pub qid: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct PagerCommandResponded {
    pub qid: u32,
    pub resp: CompletionToKernel,
}

#[derive(Clone, Copy, Debug)]
pub struct PagerRequestRecv {
    pub req: PagerRequest,
    pub qid: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct PagerRequestCompleted {
    pub qid: u32,
    pub resp: CompletionToPager,
}

pub trait TraceDataCast {
    const EVENT: u64;
}

impl TraceDataCast for ContextMapEvent {
    const EVENT: u64 = CONTEXT_MAP;
}

impl TraceDataCast for ContextFaultEvent {
    const EVENT: u64 = CONTEXT_FAULT;
}

impl TraceDataCast for ThreadEvent {
    const EVENT: u64 = THREAD_EXIT;
}

impl TraceDataCast for ThreadCtxSwitch {
    const EVENT: u64 = THREAD_CONTEXT_SWITCH;
}

impl TraceDataCast for ThreadMigrate {
    const EVENT: u64 = THREAD_MIGRATE;
}

impl TraceDataCast for PagerCommandSent {
    const EVENT: u64 = PAGER_COMMAND_SEND;
}

impl TraceDataCast for PagerCommandResponded {
    const EVENT: u64 = PAGER_COMMAND_RESPONDED;
}

impl TraceDataCast for PagerRequestRecv {
    const EVENT: u64 = PAGER_REQUEST_RECV;
}

impl TraceDataCast for PagerRequestCompleted {
    const EVENT: u64 = PAGER_REQUEST_COMPLETED;
}

impl TraceDataCast for SyscallEntryEvent {
    const EVENT: u64 = THREAD_SYSCALL_ENTRY;
}
