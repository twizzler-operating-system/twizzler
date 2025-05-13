use alloc::{collections::btree_map::BTreeMap, vec::Vec};
use core::ops::Range;

use nonoverlapping_interval_tree::NonOverlappingIntervalTree;
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
};

use super::ObjectPageProvider;
use crate::{
    arch::VirtAddr,
    memory::{
        context::ObjectContextInfo,
        pagetables::{MappingCursor, MappingFlags, MappingSettings},
    },
    obj::{pages::Page, ObjectRef},
};

#[derive(Clone, Debug, PartialEq)]
pub struct MapRegion {
    pub object: ObjectRef,
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

    pub(super) fn phys_provider<'a>(&self, page: &'a Page) -> ObjectPageProvider<'a> {
        ObjectPageProvider { page }
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
