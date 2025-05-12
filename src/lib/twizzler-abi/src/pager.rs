use bitflags::bitflags;
use twizzler_rt_abi::{error::RawTwzError, object::ObjID};

use crate::{
    object::NULLPAGE_SIZE,
    syscall::{BackingType, LifetimeType},
};

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

    pub fn id(&self) -> Option<ObjID> {
        match self.cmd() {
            KernelCommand::PageDataReq(objid, _) => Some(objid),
            KernelCommand::ObjectInfoReq(objid) => Some(objid),
            KernelCommand::ObjectEvict(info) => Some(info.obj_id),
            KernelCommand::ObjectDel(objid) => Some(objid),
            KernelCommand::ObjectCreate(objid, _) => Some(objid),
            KernelCommand::DramPages(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum KernelCommand {
    PageDataReq(ObjID, ObjectRange),
    ObjectInfoReq(ObjID),
    ObjectEvict(ObjectEvictInfo),
    ObjectDel(ObjID),
    ObjectCreate(ObjID, ObjectInfo),
    DramPages(PhysRange),
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct CompletionToKernel {
    data: KernelCompletionData,
    flags: KernelCompletionFlags,
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
    pub struct KernelCompletionFlags: u32 {
        const DONE = 1;
    }
}

impl CompletionToKernel {
    pub fn new(data: KernelCompletionData, flags: KernelCompletionFlags) -> Self {
        Self { data, flags }
    }

    pub fn data(&self) -> KernelCompletionData {
        self.data
    }

    pub fn flags(&self) -> KernelCompletionFlags {
        self.flags
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum KernelCompletionData {
    Okay,
    Error(RawTwzError),
    PageDataCompletion(ObjID, ObjectRange, PhysRange),
    ObjectInfoCompletion(ObjID, ObjectInfo),
}

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

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum PagerRequest {
    Ready,
    CopyUserPhys {
        target_object: ObjID,
        offset: usize,
        len: usize,
        phys: PhysRange,
        write_phys: bool,
    },
}

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

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum PagerCompletionData {
    Okay,
    Error(RawTwzError),
    DramPages(PhysRange),
}

pub struct PageDataReq {
    pub objid: ObjID,
    pub object_range: ObjectRange,
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct ObjectInfo {
    pub lifetime: LifetimeType,
    pub backing: BackingType,
}

impl ObjectInfo {
    pub fn new(lifetime: LifetimeType) -> Self {
        Self {
            lifetime,
            backing: BackingType::Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct PhysRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct ObjectRange {
    pub start: u64,
    pub end: u64,
}

impl PhysRange {
    pub fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    pub fn len(&self) -> usize {
        (self.end - self.start) as usize
    }

    pub fn pages(&self) -> impl Iterator<Item = u64> {
        let first_page = self.start / NULLPAGE_SIZE as u64;
        let last_page = self.end / NULLPAGE_SIZE as u64;
        first_page..last_page
    }
}

impl ObjectRange {
    pub fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    pub fn len(&self) -> usize {
        (self.end - self.start) as usize
    }

    pub fn pages(&self) -> impl Iterator<Item = u64> {
        let first_page = self.start / NULLPAGE_SIZE as u64;
        let last_page = self.end / NULLPAGE_SIZE as u64;
        first_page..last_page
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct ObjectEvictInfo {
    pub obj_id: ObjID,
    pub range: ObjectRange,
    pub phys: PhysRange,
    pub flags: ObjectEvictFlags,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
    pub struct ObjectEvictFlags: u32 {
        const SYNC = 1;
        const FENCE = 2;
    }
}
