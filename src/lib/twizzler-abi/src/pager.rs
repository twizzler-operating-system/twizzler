use core::fmt::Debug;

use bitflags::bitflags;
use twizzler_rt_abi::{
    error::RawTwzError,
    object::{ObjID, Protections},
};

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

    pub fn set_flags(&mut self, flags: KernelCompletionFlags) {
        self.flags |= flags;
    }

    pub fn clear_flags(&mut self, flags: KernelCompletionFlags) {
        self.flags &= !flags;
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
    pub kuid: ObjID,
    pub nonce: u128,
    pub def_prot: Protections,
}

impl ObjectInfo {
    pub fn new(
        lifetime: LifetimeType,
        backing: BackingType,
        kuid: ObjID,
        nonce: u128,
        def_prot: Protections,
    ) -> Self {
        Self {
            lifetime,
            backing,
            kuid,
            nonce,
            def_prot,
        }
    }
}

#[derive(Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct PhysRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct ObjectRange {
    pub start: u64,
    pub end: u64,
}

impl Debug for ObjectRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ObjRange[{:x} - {:x})", self.start, self.end)
    }
}
impl Debug for PhysRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PhyRange[{:x} - {:x})", self.start, self.end)
    }
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
    pub version: u64,
    pub flags: ObjectEvictFlags,
}

impl ObjectEvictInfo {
    pub fn new(
        obj_id: ObjID,
        range: ObjectRange,
        phys: PhysRange,
        version: u64,
        flags: ObjectEvictFlags,
    ) -> Self {
        Self {
            obj_id,
            range,
            phys,
            version,
            flags,
        }
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
    pub struct ObjectEvictFlags: u32 {
        const SYNC = 1;
        const FENCE = 2;
    }
}
