use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use core::{fmt::Debug, ops::Range, usize};

use nonoverlapping_interval_tree::NonOverlappingIntervalTree;
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections, MAX_SIZE},
    syscall::{MapControlCmd, MapFlags, SyncFlags, ThreadSyncReference, ThreadSyncWake},
    upcall::{
        MemoryAccessKind, MemoryContextViolationInfo, ObjectMemoryError, ObjectMemoryFaultInfo,
        UpcallInfo,
    },
};
use twizzler_rt_abi::error::{IoError, RawTwzError, TwzError};

use super::ObjectPageProvider;
use crate::{
    arch::VirtAddr,
    memory::{
        context::ObjectContextInfo,
        frame::PHYS_LEVEL_LAYOUTS,
        pagetables::{MappingCursor, MappingFlags, MappingSettings},
        tracker::{FrameAllocFlags, FrameAllocator},
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
        cause: MemoryAccessKind,
        perms: PermsInfo,
        default_prot: Protections,
        mapper: impl FnOnce(PageNumber, ObjectPageProvider) -> Result<(), UpcallInfo>,
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
                return mapper(
                    PageNumber::from_address(addr),
                    ObjectPageProvider::new(Vec::from([(page, settings)])),
                );
            }
        }

        let mut obj_page_tree = self.object.lock_page_tree();
        obj_page_tree = self.object.ensure_in_core(obj_page_tree, page_number);

        let mut status = obj_page_tree.get_page(page_number, get_page_flags, Some(&mut fa));
        if matches!(status, PageStatus::NoPage) && !self.object.use_pager() {
            if let Some(frame) = fa.try_allocate() {
                let page = Page::new(frame);
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
            return self.map(addr, cause, perms, default_prot, mapper);
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
                    log::debug!(
                        "adding persist dirty page {} to region {:?}, obj {:?}",
                        page_number,
                        self.range,
                        self.object().id(),
                    );
                }
                self.object().dirty_set().add_dirty(page_number);
            }

            let addr_aligned = addr
                .align_down(PHYS_LEVEL_LAYOUTS[1].size() as u64)
                .unwrap();
            let large_aligned_pn = PageNumber::from_address(addr_aligned);
            let large_diff = PageNumber::from_address(addr) - large_aligned_pn;
            let phys_aligned = page
                .physical_address()
                .align_down(PHYS_LEVEL_LAYOUTS[1].size() as u64)
                .unwrap();
            let aligned = (page.physical_address() - phys_aligned) == (addr - addr_aligned);
            if page.page_offset() >= large_diff
                && large_diff > 0
                && !settings.perms().contains(Protections::WRITE)
                && self.flags.contains(MapFlags::NO_NULLPAGE)
                && aligned
                && page.nr_pages() >= PHYS_LEVEL_LAYOUTS[1].size() / PHYS_LEVEL_LAYOUTS[0].size()
            {
                log::trace!(
                    "ADJUST: {} {} {} {}: {:?}",
                    page.page_offset(),
                    PageNumber::from_address(addr),
                    large_aligned_pn,
                    large_diff,
                    page,
                );
                mapper(
                    large_aligned_pn,
                    ObjectPageProvider::new(Vec::from([(page.adjust_down(large_diff), settings)])),
                )
            } else {
                mapper(
                    PageNumber::from_address(addr),
                    ObjectPageProvider::new(Vec::from([(page, settings)])),
                )
            }
        } else {
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
                    log::debug!(
                        "sync region {:?} with dirty pages {:?}",
                        self.range,
                        dirty_pages
                    );
                    if self.object().use_pager() && !dirty_pages.is_empty() {
                        crate::pager::sync_region(self, dirty_pages, sync_info, version);
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
