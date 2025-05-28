use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec::Vec};
use core::ops::Range;

use nonoverlapping_interval_tree::NonOverlappingIntervalTree;
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
    upcall::{MemoryAccessKind, ObjectMemoryError, ObjectMemoryFaultInfo, UpcallInfo},
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
    obj::{
        pages::Page,
        range::{PageRangeTree, PageStatus},
        ObjectRef, PageNumber,
    },
    security::PermsInfo,
};

#[derive(Clone)]
pub struct MapRegion {
    pub object: ObjectRef,
    pub shadow: Option<Arc<Shadow>>,
    pub offset: u64,
    pub cache_type: CacheType,
    pub prot: Protections,
    pub range: Range<VirtAddr>,
}

impl From<&MapRegion> for ObjectContextInfo {
    fn from(value: &MapRegion) -> Self {
        ObjectContextInfo {
            object: value.object.clone(),
            cache: value.cache_type,
            perms: value.prot,
        }
    }
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
        mapper: impl FnOnce(ObjectPageProvider) -> Result<(), UpcallInfo>,
    ) -> Result<(), UpcallInfo> {
        let page_number = PageNumber::from_address(addr);
        let is_kern_obj = addr.is_kernel_object_memory();
        let mut fa = FrameAllocator::new(
            FrameAllocFlags::ZEROED | FrameAllocFlags::WAIT_OK,
            PHYS_LEVEL_LAYOUTS[0],
        );

        let mut obj_page_tree = self.object.lock_page_tree();
        obj_page_tree = self.object.ensure_in_core(obj_page_tree, page_number);
        let mut status =
            obj_page_tree.get_page(page_number, cause == MemoryAccessKind::Write, Some(&mut fa));
        if matches!(status, PageStatus::NoPage) && !self.object.use_pager() {
            if let Some(frame) = fa.try_allocate() {
                let page = Page::new(frame);
                obj_page_tree.add_page(page_number, page, Some(&mut fa));
            }
            status = obj_page_tree.get_page(
                page_number,
                cause == MemoryAccessKind::Write,
                Some(&mut fa),
            );
            if matches!(status, PageStatus::NoPage) {
                logln!("spuriously failed to back volatile object with DRAM -- retrying fault");
                return Ok(());
            }
        }

        // Step 4: do the mapping. If the page isn't present by now, report data loss.
        if let PageStatus::Ready(page, offset, cow) = status {
            let settings = self.mapping_settings(cow, is_kern_obj);
            let settings = MappingSettings::new(
                // Provided permissions, restricted by mapping.
                (perms.provide | default_prot) & !perms.restrict & settings.perms(),
                settings.cache(),
                settings.flags(),
            );
            mapper(ObjectPageProvider::new(Vec::from([(
                page, offset, settings,
            )])))
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
    tree: PageRangeTree,
}
