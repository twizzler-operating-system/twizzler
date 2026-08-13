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

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct PagerFlags : u32 {
        const PREFETCH = 1;
        /// Raised on behalf of a thread below the default userspace priority. The pager keeps
        /// such requests off the lanes it reserves for demand faults; an unflagged request is
        /// treated as demand work.
        const BACKGROUND = 2;
    }
}

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
            KernelCommand::PageDataReq(objid, _, _, _) => Some(objid),
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
    /// Page in a range of an object.
    ///
    /// The third range is the *required* subrange: the pages a thread is actually blocked on, as
    /// opposed to the widening `ensure_in_core_pager` adds around them to install a large page and
    /// save later faults. The pager transfers and completes it first, so the waiter can be woken
    /// after tens of kilobytes rather than after the whole request (`pagerperf.md` 11) -- except
    /// where cutting the transfer that small would cost the region its large-page merge, which the
    /// pager decides for itself (`largepager.md`). Empty means
    /// "no part of this is more urgent than any other" -- a prefetch, or a caller that needs all
    /// of it -- and the request is served in address order.
    PageDataReq(ObjID, ObjectRange, PagerFlags, ObjectRange),
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

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
    pub struct PageFlags: u32 {
        const WIRED = 0x1;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub enum KernelCompletionData {
    Okay,
    Error(RawTwzError),
    PageDataCompletion(ObjID, ObjectRange, PhysRange, PageFlags),
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
    RegisterPhys(u64, u64),
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

bitflags::bitflags! {
    /// Extra guarantees a pager can attach to an [ObjectInfo] it returns.
    ///
    /// Both are about the object's *meta page*, which the kernel otherwise has to fault in on the
    /// first `check_id` -- a full page-data round trip charged to whoever is mapping the object.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct ObjectInfoFlags : u32 {
        /// `meta_page` holds physical pages the pager has already filled with the object's meta
        /// page. The kernel installs them directly, so nothing has to page it in later.
        const META_PAGE = 1;
        /// The pager has checked that this object's ID matches its metadata, so the kernel may
        /// take `kuid`/`nonce`/`def_prot` as the object's real values rather than re-deriving
        /// them. Independent of `META_PAGE`: a pager that streams a page it never reads (the
        /// store writes straight into the physical page) can set one and not the other.
        const VALIDATED = 2;
        /// This object's whole meta page is derivable from the fields here plus `size`, so the
        /// kernel should build it rather than have one sent. For an external file there is nothing
        /// to send -- the pager invents the metadata from the file's length -- and building it
        /// kernel-side avoids both a `CopyUserPhys` and a later fault. Mutually exclusive with
        /// `META_PAGE` in practice; `META_PAGE` wins if both are set.
        const SYNTH_META = 4;
        /// `size` is the object's length in the store, and the pager promises there is nothing
        /// beyond it to read.
        ///
        /// A positive assertion rather than an inference, because the kernel acts on it by
        /// answering faults past that point *without asking the pager at all*. Zero is a perfectly
        /// good length, so "size == 0" cannot distinguish an empty object from a pager that simply
        /// never filled the field -- and reading the second as the first serves zeros over real
        /// data. It did: setting the kernel's length from an unflagged `size` made every stored
        /// object look empty, and the guest died with `failed to enumerate dependencies for
        /// libtwz_rt.so` because its libraries read back as zeros (`pagerperf.md` 20).
        const SIZE_VALID = 8;
    }
}

/// Kernel-facing description of an object.
///
/// Also travels the other way, in [KernelCommand::ObjectCreate]; `flags` and `meta_page` are
/// meaningless in that direction and are left empty.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct ObjectInfo {
    pub lifetime: LifetimeType,
    pub backing: BackingType,
    pub kuid: ObjID,
    pub nonce: u128,
    pub def_prot: Protections,
    pub flags: ObjectInfoFlags,
    /// Physical memory holding the object's meta page. Only meaningful with
    /// [ObjectInfoFlags::META_PAGE].
    pub meta_page: PhysRange,
    /// The object's data length, for the `MEXT_SIZED` meta extension. Only meaningful with
    /// [ObjectInfoFlags::SYNTH_META].
    pub size: u64,
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
            flags: ObjectInfoFlags::empty(),
            meta_page: PhysRange::new(0, 0),
            size: 0,
        }
    }

    /// Attach a pager-filled meta page.
    pub fn with_meta_page(mut self, meta_page: PhysRange) -> Self {
        self.flags |= ObjectInfoFlags::META_PAGE;
        self.meta_page = meta_page;
        self
    }

    /// Ask the kernel to build the meta page itself, for an object `size` bytes long.
    ///
    /// Also states the length, since building the page from it is a strictly stronger claim.
    pub fn synth_meta(mut self, size: u64) -> Self {
        self.flags |= ObjectInfoFlags::SYNTH_META;
        self.size = size;
        self.with_size(size)
    }

    /// State the object's length in the store: there is nothing past `size` to read.
    pub fn with_size(mut self, size: u64) -> Self {
        self.flags |= ObjectInfoFlags::SIZE_VALID;
        self.size = size;
        self
    }

    /// Assert that this info's fields are the object's real metadata.
    pub fn validated(mut self) -> Self {
        self.flags |= ObjectInfoFlags::VALIDATED;
        self
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

    pub fn pages(&self) -> impl Iterator<Item = u64> + DoubleEndedIterator {
        let first_page = self.start / NULLPAGE_SIZE as u64;
        let last_page = self.end / NULLPAGE_SIZE as u64;
        first_page..last_page
    }

    pub fn page_count(&self) -> usize {
        let first_page = self.start / NULLPAGE_SIZE as u64;
        let last_page = self.end / NULLPAGE_SIZE as u64;
        (last_page - first_page) as usize
    }
}

impl core::ops::Add<u64> for PhysRange {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        if rhs == 0 {
            Self::new(self.start, self.end)
        } else {
            Self::new(self.end, self.end + NULLPAGE_SIZE as u64)
        }
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

    pub fn page_count(&self) -> usize {
        let first_page = self.start / NULLPAGE_SIZE as u64;
        let last_page = self.end / NULLPAGE_SIZE as u64;
        (last_page - first_page) as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct ObjectEvictInfo {
    pub obj_id: ObjID,
    pub range: ObjectRange,
    pub phys: PhysRange,
    pub version: u64,
    pub flags: ObjectEvictFlags,
    pub uniq_id: ObjID,
}

impl ObjectEvictInfo {
    pub fn new(
        obj_id: ObjID,
        range: ObjectRange,
        phys: PhysRange,
        version: u64,
        flags: ObjectEvictFlags,
        uniq_id: ObjID,
    ) -> Self {
        Self {
            uniq_id,
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
