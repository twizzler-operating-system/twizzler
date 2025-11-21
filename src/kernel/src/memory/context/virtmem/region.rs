use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use core::{fmt::Debug, ops::Range, sync::atomic::Ordering, usize};

use nonoverlapping_interval_tree::NonOverlappingIntervalTree;
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections, MAX_SIZE},
    syscall::{MapControlCmd, MapFlags, SyncFlags, ThreadSyncReference, ThreadSyncWake, TimeSpan},
    trace::{ContextFaultEvent, FaultFlags, TraceEntryFlags, TraceKind, CONTEXT_FAULT},
    upcall::{
        MemoryAccessKind, MemoryContextViolationInfo, ObjectMemoryError, ObjectMemoryFaultInfo,
        UpcallInfo,
    },
};
use twizzler_rt_abi::error::{IoError, RawTwzError, TwzError};

use super::{ObjectPageProvider, PageFaultFlags, MAX_OPP_VEC};
use crate::{
    arch::VirtAddr,
    instant::Instant,
    memory::{
        context::ObjectContextInfo,
        frame::PHYS_LEVEL_LAYOUTS,
        pagetables::{
            MappingCursor, MappingFlags, MappingSettings, PhysAddrProvider, SharedPageTable,
        },
        tracker::{FrameAllocFlags, FrameAllocator},
        FAULT_STATS,
    },
    mutex::Mutex,
    obj::{
        copy::copy_range_to_shadow,
        pages::{Page, PageRef},
        range::{GetPageFlags, PageRangeTree, PageStatus},
        ObjectRef, PageNumber,
    },
    security::PermsInfo,
    syscall::sync::wakeup,
    thread::{current_memory_context, current_thread_ref},
    trace::{
        mgr::{TraceEvent, TRACE_MGR},
        new_trace_entry,
    },
};

#[derive(Clone)]
pub struct MapRegion {
    pub object: ObjectRef,
    pub shadow: Option<Arc<Shadow>>,
    pub offset: u64,
    pub cache_type: CacheType,
    pub prot: Protections,
    pub flags: MapFlags,
    pub range: Range<VirtAddr>,
    pub shared_pt: Option<SharedPageTable>,
}

impl From<&MapRegion> for ObjectContextInfo {
    fn from(value: &MapRegion) -> Self {
        ObjectContextInfo {
            object: value.object.clone(),
            cache: value.cache_type,
            perms: value.prot,
            flags: value.flags,
        }
    }
}

fn check_settings(
    addr: VirtAddr,
    settings: &MappingSettings,
    kind: MemoryAccessKind,
) -> Result<(), UpcallInfo> {
    if !settings.flags().contains(MappingFlags::USER) {
        return Ok(());
    }
    let upcall =
        UpcallInfo::MemoryContextViolation(MemoryContextViolationInfo::new(addr.raw(), kind));
    match kind {
        MemoryAccessKind::Read => {
            if !settings.perms().contains(Protections::READ) {
                return Err(upcall);
            }
        }
        MemoryAccessKind::Write => {
            if !settings.perms().contains(Protections::WRITE) {
                return Err(upcall);
            }
        }
        MemoryAccessKind::InstructionFetch => {
            if !settings.perms().contains(Protections::EXEC) {
                return Err(upcall);
            }
        }
    }
    Ok(())
}

impl MapRegion {
    fn trace_fault(
        &self,
        addr: VirtAddr,
        ip: VirtAddr,
        cause: MemoryAccessKind,
        pfflags: PageFaultFlags,
        used_pager: bool,
        large: bool,
        start_time: Instant,
    ) {
        if ip.is_kernel() || addr.is_kernel_object_memory() {
            return;
        }
        if TRACE_MGR.any_enabled(TraceKind::Context, CONTEXT_FAULT) {
            let mut flags = FaultFlags::empty();
            match cause {
                MemoryAccessKind::Read => flags.insert(FaultFlags::READ),
                MemoryAccessKind::Write => flags.insert(FaultFlags::WRITE),
                MemoryAccessKind::InstructionFetch => flags.insert(FaultFlags::EXEC),
            }
            if pfflags.contains(PageFaultFlags::USER) {
                flags.insert(FaultFlags::USER);
            }
            if large {
                flags.insert(FaultFlags::LARGE);
            }
            if used_pager {
                flags.insert(FaultFlags::PAGER);
            }

            let processing_time = Instant::now()
                .checked_sub_instant(&start_time)
                .map(|d| TimeSpan::from_nanos(d.as_nanos() as u64))
                .unwrap_or(TimeSpan::ZERO);
            let data = ContextFaultEvent {
                addr: addr.raw(),
                obj: self.object().id(),
                flags,
                processing_time,
            };
            let entry =
                new_trace_entry(TraceKind::Context, CONTEXT_FAULT, TraceEntryFlags::HAS_DATA);

            TRACE_MGR.enqueue(TraceEvent::new_with_data(entry, data));
        }
    }

    pub fn mapping_cursor(&self, start: usize, len: usize) -> MappingCursor {
        MappingCursor::new(self.range.start.offset(start).unwrap(), len)
    }

    pub fn mapping_settings(&self, wp: bool, is_kern_obj: bool) -> MappingSettings {
        let mut prot = self.prot;
        if wp {
            prot.remove(Protections::WRITE);
        }
        MappingSettings::new(
            prot,
            self.cache_type,
            if is_kern_obj {
                MappingFlags::GLOBAL
            } else {
                MappingFlags::USER
            },
        )
    }

    pub fn object(&self) -> &ObjectRef {
        &self.object
    }

    pub(super) fn map(
        &self,
        addr: VirtAddr,
        ip: VirtAddr,
        cause: MemoryAccessKind,
        pfflags: PageFaultFlags,
        perms: PermsInfo,
        default_prot: Protections,
        start_time: Instant,
        mapper: impl FnOnce(
            Option<&SharedPageTable>,
            PageNumber,
            ObjectPageProvider,
        ) -> Result<(), UpcallInfo>,
        shared_mapper: impl Fn(VirtAddr, &SharedPageTable) -> Result<(), UpcallInfo>,
    ) -> Result<(), UpcallInfo> {
        let mut page_number = PageNumber::from_address(addr);
        if self.flags.contains(MapFlags::NO_NULLPAGE) && !page_number.is_meta() {
            page_number = page_number.offset(1);
        }

        let is_kern_obj = addr.is_kernel_object_memory();
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK,
            PHYS_LEVEL_LAYOUTS[0],
        );
        let get_page_flags = if cause == MemoryAccessKind::Write {
            GetPageFlags::WRITE
        } else {
            GetPageFlags::empty()
        };

        if let Some(shared_pt) = &self.shared_pt
            && !is_kern_obj
        {
            log::trace!(
                "shared map for: {}: {:?} {:?}: {:?}",
                self.object().id(),
                addr,
                cause,
                shared_pt.provider().peek().unwrap().addr
            );
            check_settings(addr, &shared_pt.settings, cause)?;
            shared_mapper(addr, shared_pt)?;
            shared_pt.inc_refs();
        }

        if let Some(shadow) = &self.shadow {
            if let Some(page) = shadow.get_page(page_number, get_page_flags) {
                let settings = self.mapping_settings(true, is_kern_obj);
                let settings = MappingSettings::new(
                    // Provided permissions, restricted by mapping.
                    (perms.provide | default_prot) & !perms.restrict & settings.perms(),
                    settings.cache(),
                    settings.flags(),
                );
                check_settings(addr, &settings, cause)?;
                self.trace_fault(addr, ip, cause, pfflags, false, false, start_time);
                return mapper(
                    self.shared_pt.as_ref(),
                    PageNumber::from_address(addr),
                    ObjectPageProvider::new(heapless::Vec::<_, MAX_OPP_VEC>::from([(
                        page, settings,
                    )])),
                );
            }
        }

        let mut obj_page_tree = self.object.lock_page_tree();
        let mut used_pager = false;
        obj_page_tree = self
            .object
            .ensure_in_core(obj_page_tree, page_number, &mut used_pager);

        let mut status = obj_page_tree.get_page(page_number, get_page_flags, Some(&mut fa));
        if matches!(status, PageStatus::NoPage) && !self.object.use_pager() {
            log::warn!("fallback allocate in fault to page {}", page_number);
            if let Some(frame) = fa.try_allocate() {
                let page = Page::new(frame, 1);
                obj_page_tree.add_page(
                    page_number,
                    PageRef::new(Arc::new(page), 0, 1),
                    Some(&mut fa),
                );
            }
            status = obj_page_tree.get_page(page_number, get_page_flags, Some(&mut fa));
            if matches!(status, PageStatus::NoPage) {
                logln!("spuriously failed to back volatile object with DRAM -- retrying fault");
                return Ok(());
            }
        }

        if let PageStatus::Locked(sleeper) = status {
            drop(obj_page_tree);
            sleeper.wait();
            return self.map(
                addr,
                ip,
                cause,
                pfflags,
                perms,
                default_prot,
                start_time,
                mapper,
                shared_mapper,
            );
        }

        // Step 4: do the mapping. If the page isn't present by now, report data loss.
        if let PageStatus::Ready(page, shared) = status {
            let settings = self.mapping_settings(shared, is_kern_obj);
            let settings = MappingSettings::new(
                // Provided permissions, restricted by mapping.
                (perms.provide | default_prot) & !perms.restrict & settings.perms(),
                settings.cache(),
                settings.flags(),
            );
            check_settings(addr, &settings, cause)?;
            if settings.perms().contains(Protections::WRITE) {
                if self.object().use_pager() {
                    log::trace!(
                        "adding persist dirty page {} to region {:?}, obj {:?}",
                        page_number,
                        self.range,
                        self.object().id(),
                    );
                    self.object()
                        .dirty_set()
                        .add_dirty(page_number, page.nr_pages());
                }
            }

            let pages_per_large = PHYS_LEVEL_LAYOUTS[1].size() / PHYS_LEVEL_LAYOUTS[0].size();
            let large_page_number = page_number.align_down(pages_per_large);
            let mut large_diff = page_number - large_page_number;

            let phys_large_aligned = page
                .physical_address()
                .align_down(PHYS_LEVEL_LAYOUTS[1].size() as u64)
                .unwrap();
            let addr_large_aligned = addr
                .align_down(PHYS_LEVEL_LAYOUTS[1].size() as u64)
                .unwrap();

            let phys_page_aligned = page
                .physical_address()
                .align_down(PHYS_LEVEL_LAYOUTS[0].size() as u64)
                .unwrap();
            let addr_page_aligned = addr
                .align_down(PHYS_LEVEL_LAYOUTS[0].size() as u64)
                .unwrap();

            let aligned = (phys_page_aligned - phys_large_aligned)
                == (addr_page_aligned - addr_large_aligned);
            if self.flags.contains(MapFlags::NO_NULLPAGE)
                && !page_number.is_meta()
                && large_diff > 0
            {
                large_diff -= 1;
            }
            if page.nr_pages() > 1 {
                log::trace!(
                    "possible bigmap {:?}: {} {}: {}, {}, {:?} {} {}",
                    ip,
                    page_number,
                    large_page_number,
                    page.page_offset(),
                    page.nr_pages(),
                    addr,
                    aligned,
                    large_diff
                );
            }

            if page.page_offset() >= large_diff
                && large_diff > 0
                && aligned
                && !addr.is_kernel()
                && !addr.is_kernel_object_memory()
                && page.nr_pages() + large_diff >= pages_per_large
            {
                FAULT_STATS.count[1].fetch_add(1, Ordering::SeqCst);
                log::trace!(
                    "map large page {} for page {}. phys: {:?}, diff: {}. {:?} {:?} {:?} {:?}: {}",
                    large_page_number,
                    page_number,
                    page.physical_address(),
                    large_diff,
                    addr_page_aligned,
                    phys_page_aligned,
                    addr_page_aligned - addr_large_aligned,
                    phys_page_aligned - phys_large_aligned,
                    aligned
                );
                let ret = mapper(
                    self.shared_pt.as_ref(),
                    large_page_number,
                    ObjectPageProvider::new(heapless::Vec::from([(
                        page.adjust_down(large_diff),
                        settings,
                    )])),
                );
                drop(obj_page_tree);
                if ret.is_ok() {
                    self.trace_fault(addr, ip, cause, pfflags, used_pager, true, start_time);
                }
                ret
            } else {
                FAULT_STATS.count[0].fetch_add(1, Ordering::SeqCst);

                let mut provider = ObjectPageProvider::new(heapless::Vec::from([(page, settings)]));
                if cause != MemoryAccessKind::Write
                    && !settings.perms().contains(Protections::WRITE)
                {
                    let mut pages = heapless::Vec::<_, MAX_OPP_VEC>::new();
                    if obj_page_tree
                        .try_get_pages(page_number, &mut pages, settings)
                        .is_some()
                        && !pages.is_empty()
                    {
                        log::trace!(
                            "mapping multiple pages for {}: {}, {}",
                            self.object().id(),
                            pages.len(),
                            pages.iter().fold(0, |acc, p| acc + p.0.nr_pages())
                        );
                        provider = ObjectPageProvider::new(pages);
                    }
                };

                let ret = mapper(
                    self.shared_pt.as_ref(),
                    PageNumber::from_address(addr),
                    provider,
                );
                drop(obj_page_tree);
                if ret.is_ok() {
                    self.trace_fault(addr, ip, cause, pfflags, used_pager, false, start_time);
                }
                ret
            }
        } else {
            log::warn!(
                "failed to get page {} for object {} due to page fault {:x} {:?} {:?}",
                page_number,
                self.object().id(),
                addr.raw(),
                cause,
                pfflags
            );
            Err(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
                self.object().id(),
                ObjectMemoryError::BackingFailed(RawTwzError::new(
                    TwzError::Io(IoError::DataLoss).raw(),
                )),
                cause,
                addr.raw() as usize,
            )))
        }
    }

    pub fn ctrl(&self, cmd: MapControlCmd, _opts: u64) -> Result<u64, TwzError> {
        match cmd {
            MapControlCmd::Sync(sync_info_ptr) => {
                // TODO: validation
                let sync_info = unsafe { sync_info_ptr.read() };
                let version = sync_info.release_compare;

                if sync_info.flags.contains(SyncFlags::DURABLE) {
                    let dirty_pages = self.object().dirty_set().drain_all();
                    log::trace!(
                        "sync region {:?} with dirty pages {:?}",
                        self.range,
                        dirty_pages
                    );
                    if self.object().use_pager() && !dirty_pages.is_empty() {
                        crate::pager::sync_region(self, dirty_pages.as_slice(), sync_info, version);
                    }
                }

                if sync_info.flags.contains(SyncFlags::ASYNC_DURABLE) {
                    unsafe { sync_info.try_release() }?;
                    let wake = ThreadSyncWake::new(
                        ThreadSyncReference::Virtual(sync_info.release),
                        usize::MAX,
                    );
                    wakeup(&wake)?;
                }

                Ok(0)
            }
            MapControlCmd::Discard => {
                todo!()
            }
            MapControlCmd::Invalidate => {
                let ctx = current_memory_context().unwrap();
                ctx.with_arch(current_thread_ref().unwrap().secctx.active_id(), |arch| {
                    let cursor = self.mapping_cursor(0, MAX_SIZE);
                    arch.unmap(cursor);
                });
                Ok(0)
            }
            MapControlCmd::Update => {
                let info = ObjectContextInfo {
                    object: self.object.clone(),
                    perms: self.prot,
                    cache: self.cache_type,
                    flags: self.flags,
                };
                if let Some(shadow) = &self.shadow {
                    shadow.update(&info);
                }
                let ctx = current_memory_context().unwrap();
                ctx.with_arch(current_thread_ref().unwrap().secctx.active_id(), |arch| {
                    let cursor = self.mapping_cursor(0, MAX_SIZE);
                    arch.unmap(cursor);
                });
                Ok(0)
            }
        }
    }
}

#[derive(Default)]
pub struct RegionManager {
    tree: NonOverlappingIntervalTree<VirtAddr, MapRegion>,
    objects: BTreeMap<ObjID, Vec<Range<VirtAddr>>>,
}

impl RegionManager {
    pub fn insert_region(&mut self, region: MapRegion) {
        let object_entry = self.objects.entry(region.object.id()).or_default();
        let range = region.range.clone();
        let old = self.tree.insert_replace(range.clone(), region);
        for old_region in old {
            let pos = object_entry
                .iter()
                .position(|item| item == &old_region.0)
                .expect("failed to find object range");
            object_entry.swap_remove(pos);
        }
        object_entry.push(range);
    }

    pub fn remove_region(&mut self, addr: VirtAddr) -> Option<MapRegion> {
        if let Some(region) = self.tree.remove(&addr) {
            let object_entry = self.objects.entry(region.object.id()).or_default();
            let pos = object_entry
                .iter()
                .position(|item| item == &region.range)
                .expect("failed to find object range");
            object_entry.swap_remove(pos);
            Some(region)
        } else {
            None
        }
    }

    pub fn lookup_region(&mut self, addr: VirtAddr) -> Option<&MapRegion> {
        self.tree.get(&addr)
    }

    pub fn object_mappings(&mut self, id: ObjID) -> impl Iterator<Item = &MapRegion> {
        self.objects.entry(id).or_default().iter().map(|info| {
            self.tree
                .get(&info.start)
                .expect("failed to lookup mapping")
        })
    }

    pub fn mappings(&self) -> impl Iterator<Item = &MapRegion> {
        self.tree.iter().map(|x| x.1.value())
    }

    pub fn objects(&self) -> impl Iterator<Item = &ObjID> {
        self.objects.keys().into_iter()
    }
}

pub struct Shadow {
    tree: Mutex<PageRangeTree>,
}

impl Debug for Shadow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Shadow {{..}}")
    }
}

impl Shadow {
    pub fn new(info: &ObjectContextInfo) -> Self {
        let mut tree = PageRangeTree::new(info.object().id());
        log::debug!("copy range to shadow {:?}", info.object().id());
        copy_range_to_shadow(&info.object, 0, &mut tree, 0, MAX_SIZE);
        Self {
            tree: Mutex::new(tree),
        }
    }

    pub fn update(&self, info: &ObjectContextInfo) {
        let mut tree = self.tree.lock();
        tree.clear();
        copy_range_to_shadow(&info.object, 0, &mut *tree, 0, MAX_SIZE);
    }

    pub fn get_page(&self, pn: PageNumber, flags: GetPageFlags) -> Option<PageRef> {
        match self.tree.lock().try_get_page(pn, flags) {
            PageStatus::Ready(page_ref, _) => Some(page_ref),
            _ => None,
        }
    }

    pub fn with_page_tree<R>(&self, f: impl FnOnce(&mut PageRangeTree) -> R) -> R {
        f(&mut *self.tree.lock())
    }
}

impl From<&MapRegion> for Shadow {
    fn from(value: &MapRegion) -> Self {
        let info: ObjectContextInfo = value.into();
        Shadow::new(&info)
    }
}
