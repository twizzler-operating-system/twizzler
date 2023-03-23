use crate::{
    object::ObjID,
    syscall::{BackingType, LifetimeType},
};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct RequestFromKernel {
    cmd: KernelCommand,
}

impl RequestFromKernel {
    pub fn new(cmd: KernelCommand) -> Self {
        Self { cmd }
    }

    pub fn cmd(&self) -> KernelCommand {
        self.cmd
    }
}

pub const NUM_ENTRIES: usize = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum KernelCommand {
    ObjInfo(ObjID),
    PageData(ObjID, [ObjectRange; NUM_ENTRIES]),
    DramRel(usize),
    DramPages([PhysRange; NUM_ENTRIES]),
    Evict(ObjID, [ObjectRange; NUM_ENTRIES]),
    Sync(ObjID, [ObjectRange; NUM_ENTRIES]),
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct CompletionToKernel {
    data: KernelCompletionData,
}

impl CompletionToKernel {
    pub fn new(data: KernelCompletionData) -> Self {
        Self { data }
    }

    pub fn data(&self) -> KernelCompletionData {
        self.data
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum KernelCompletionData {
    Success,
    ObjectInfo(ObjectInfo),
    PageInfo([PhysRange; NUM_ENTRIES]),
    DramPages([PhysRange; NUM_ENTRIES]),
    Err(KernelRequestError),
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct ObjectInfo {
    pub id: ObjID,
    pub backing: BackingType,
    pub lifetime: LifetimeType,
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Default)]
#[repr(C)]
pub struct PhysRange {
    pub start: u64,
    pub len: u32,
    _flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq, Default)]
#[repr(C)]
pub struct ObjectRange {
    pub start: u64,
    pub len: u32,
    _flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum KernelRequestError {
    Unknown,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct RequestFromPager {
    cmd: PagerRequest,
}

impl RequestFromPager {
    pub fn new(cmd: PagerRequest) -> Self {
        Self { cmd }
    }

    pub fn cmd(&self) -> PagerRequest {
        self.cmd
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum PagerRequest {
    PageData(ObjID, [(PhysRange, ObjectRange); NUM_ENTRIES]),
    ObjectInfo(ObjectInfo),
    DramReq(usize),
    DramPages([PhysRange; NUM_ENTRIES]),
    Evict(ObjID, [ObjectRange; NUM_ENTRIES]),
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct CompletionToPager {
    data: PagerCompletionData,
}

impl CompletionToPager {
    pub fn new(data: PagerCompletionData) -> Self {
        Self { data }
    }

    pub fn data(&self) -> PagerCompletionData {
        self.data
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum PagerCompletionData {
    Success,
    DramPages([PhysRange; NUM_ENTRIES]),
    Err(PagerRequestErr),
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum PagerRequestErr {
    Unknown,
}
