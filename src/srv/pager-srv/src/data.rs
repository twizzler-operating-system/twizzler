use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use bitvec::prelude::*;
use miette::Result;
use twizzler_abi::pager::{
    CompletionToPager, ObjectInfo, ObjectRange, PhysRange, RequestFromPager,
};
use twizzler_object::ObjID;
use twizzler_queue::QueueSender;

use crate::helpers::{page_in, page_out, page_to_physrange, PAGE};

type PageNum = u64;

#[derive(Debug, Clone, Copy, Default)]
pub struct PerPageData {
    paddr: u64,
}

#[derive(Default)]
pub struct PerObjectInner {
    #[allow(dead_code)]
    id: ObjID,
    page_map: HashMap<PageNum, PerPageData>,
    meta_page_map: HashMap<PageNum, PerPageData>,
}

impl PerObjectInner {
    pub fn track(&mut self, obj_range: ObjectRange, phys_range: PhysRange) {
        assert_eq!(obj_range.len(), PAGE as usize);
        assert_eq!(phys_range.len(), PAGE as usize);

        if let Some(old) = self.page_map.insert(
            obj_range.start / PAGE,
            PerPageData {
                paddr: phys_range.start,
            },
        ) {
            tracing::debug!("todo: release old: {:?}", old);
        }
    }

    pub fn _track_meta(&mut self, obj_range: ObjectRange, phys_range: PhysRange) {
        assert_eq!(obj_range.len(), PAGE as usize);
        assert_eq!(phys_range.len(), PAGE as usize);

        if let Some(old) = self.meta_page_map.insert(
            obj_range.start / PAGE,
            PerPageData {
                paddr: phys_range.start,
            },
        ) {
            tracing::debug!("todo: release old: {:?}", old);
        }
    }

    pub fn lookup(&self, obj_range: ObjectRange) -> Option<PhysRange> {
        let key = obj_range.start / PAGE;
        self.page_map
            .get(&key)
            .map(|ppd| PhysRange::new(ppd.paddr, PAGE))
    }

    pub fn pages(&self) -> impl Iterator<Item = (ObjectRange, PerPageData)> + '_ {
        self.page_map
            .iter()
            .map(|(x, y)| (ObjectRange::new(*x * PAGE, (*x + 1) * PAGE), *y))
    }

    pub fn meta_pages(&self) -> impl Iterator<Item = (ObjectRange, PerPageData)> + '_ {
        self.meta_page_map
            .iter()
            .map(|(x, y)| (ObjectRange::new(*x * PAGE, (*x + 1) * PAGE), *y))
    }

    pub fn new(id: ObjID) -> Self {
        Self {
            id,
            ..Default::default()
        }
    }
}

#[derive(Clone)]
pub struct PerObject {
    id: ObjID,
    inner: Arc<Mutex<PerObjectInner>>,
}

impl PerObject {
    pub fn track(&self, obj_range: ObjectRange, phys_range: PhysRange) {
        self.inner.lock().unwrap().track(obj_range, phys_range);
    }

    pub fn lookup(&self, obj_range: ObjectRange) -> Option<PhysRange> {
        self.inner.lock().unwrap().lookup(obj_range)
    }

    pub async fn sync(
        &self,
        rq: &Arc<QueueSender<RequestFromPager, CompletionToPager>>,
    ) -> Result<()> {
        let nulls = [0; PAGE as usize];
        if object_store::write_all(self.id.raw(), &nulls, 0).is_err() {
            // TODO
            return Ok(());
        }
        let (pages, mpages) = {
            let inner = self.inner.lock().unwrap();
            let total = inner.page_map.len() + inner.meta_page_map.len();
            tracing::debug!("syncing {}: {} pages", self.id, total);
            let pages = inner.pages().collect::<Vec<_>>();
            let mpages = inner.meta_pages().collect::<Vec<_>>();
            (pages, mpages)
        };
        for p in pages {
            let phys_range = PhysRange::new(p.1.paddr, p.1.paddr + PAGE);
            tracing::trace!("sync: page: {:?} {:?}", p, phys_range);
            page_out(rq, self.id, p.0, phys_range, false).await?;
        }

        for p in mpages {
            let phys_range = PhysRange::new(p.1.paddr, p.1.paddr + PAGE);
            tracing::trace!("sync: meta page: {:?} {:?}", p, phys_range);
            page_out(rq, self.id, p.0, phys_range, true).await?;
        }

        Ok(())
    }
}

impl PerObject {
    pub fn new(id: ObjID) -> Self {
        Self {
            id,
            inner: Arc::new(Mutex::new(PerObjectInner::new(id))),
        }
    }
}

#[derive(Clone)]
pub struct PagerData {
    inner: Arc<Mutex<PagerDataInner>>,
}

pub struct PagerDataInner {
    pub bitvec: BitVec,
    pub per_obj: HashMap<ObjID, PerObject>,
    pub mem_range_start: u64,
}

impl PagerDataInner {
    /// Create a new PagerDataInner instance
    /// Initializes the data structure for managing page allocations and replacements.
    pub fn new() -> Self {
        tracing::trace!("initializing PagerDataInner");
        PagerDataInner {
            bitvec: BitVec::new(),
            per_obj: HashMap::with_capacity(0),
            mem_range_start: 0,
        }
    }

    /// Set the starting address of the memory range to be managed.
    pub fn set_range_start(&mut self, start: u64) {
        self.mem_range_start = start;
    }

    /// Get the next available page number and mark it as used.
    /// Returns the page number if available, or `None` if all pages are used.
    fn get_next_available_page(&mut self) -> Option<usize> {
        tracing::trace!("searching for next available page");
        let next_page = self.bitvec.iter().position(|bit| !bit);

        if let Some(page_number) = next_page {
            self.bitvec.set(page_number, true);
            tracing::trace!("next available page: {}", page_number);
            Some(page_number)
        } else {
            tracing::debug!("no available pages left");
            None
        }
    }

    /// Get a memory page for allocation.
    /// Triggers page replacement if all pages are used.
    fn get_mem_page(&mut self) -> usize {
        tracing::trace!("attempting to get memory page");
        if self.bitvec.all() {
            todo!()
        }
        self.get_next_available_page().expect("no available pages")
    }

    /// Remove a page from the bit vector, freeing it for future use.
    fn _remove_page(&mut self, page_number: usize) {
        tracing::trace!("attempting to remove page {}", page_number);
        if page_number < self.bitvec.len() {
            self.bitvec.set(page_number, false);
            tracing::trace!("page {} removed from bitvec", page_number);
        } else {
            tracing::warn!(
                "page {} is out of bounds and cannot be removed",
                page_number
            );
        }
    }

    /// Resize the bit vector to accommodate more pages or clear it.
    fn resize_bitset(&mut self, new_size: usize) {
        tracing::debug!("resizing bitvec to new size: {}", new_size);
        if new_size == 0 {
            tracing::trace!("clearing bitvec");
            self.bitvec.clear();
        } else {
            self.bitvec.resize(new_size, false);
        }
        tracing::trace!("bitvec resized to: {}", new_size);
    }

    /// Check if all pages are currently in use.
    pub fn _is_full(&self) -> bool {
        let full = self.bitvec.all();
        tracing::trace!("bitvec check full: {}", full);
        full
    }

    pub fn get_per_object(&mut self, id: ObjID) -> &PerObject {
        self.per_obj.entry(id).or_insert_with(|| PerObject::new(id))
    }

    pub fn get_per_object_mut(&mut self, id: ObjID) -> &mut PerObject {
        self.per_obj.entry(id).or_insert_with(|| PerObject::new(id))
    }
}

impl PagerData {
    /// Create a new PagerData instance.
    /// Wraps PagerDataInner with thread-safe access.
    pub fn new() -> Self {
        tracing::trace!("creating new PagerData instance");
        PagerData {
            inner: Arc::new(Mutex::new(PagerDataInner::new())),
        }
    }

    /// Resize the internal structures to accommodate the given number of pages.
    pub fn resize(&self, pages: usize) {
        tracing::debug!("resizing resources to support {} pages", pages);
        let mut inner = self.inner.lock().unwrap();
        inner.resize_bitset(pages);
    }

    /// Initialize the starting memory range for the pager.
    pub fn init_range(&self, range: PhysRange) {
        self.inner.lock().unwrap().set_range_start(range.start);
    }

    /// Allocate a memory page and associate it with an object and range.
    /// Page in the data from disk
    /// Returns the physical range corresponding to the allocated page.
    pub async fn fill_mem_page(
        &self,
        rq: &Arc<QueueSender<RequestFromPager, CompletionToPager>>,
        id: ObjID,
        obj_range: ObjectRange,
    ) -> Result<PhysRange> {
        {
            // See if we already allocated
            let mut inner = self.inner.lock().unwrap();
            let po = inner.get_per_object(id);
            if let Some(phys_range) = po.lookup(obj_range) {
                tracing::debug!(
                    "already paged memory page for ObjID {:?}, ObjectRange {:?}",
                    id,
                    obj_range
                );
                return Ok(phys_range);
            }
        }
        tracing::debug!(
            "allocating memory page for ObjID {:?}, ObjectRange {:?}",
            id,
            obj_range
        );
        // TODO: remove this restriction
        assert_eq!(obj_range.len(), 0x1000);
        let phys_range = {
            let mut inner = self.inner.lock().unwrap();
            let page = inner.get_mem_page();
            let phys_range = page_to_physrange(page, inner.mem_range_start);
            let po = inner.get_per_object_mut(id);
            po.track(obj_range, phys_range);
            phys_range
        };
        page_in(rq, id, obj_range, phys_range, false).await?;
        tracing::debug!("memory page allocated successfully: {:?}", phys_range);
        return Ok(phys_range);
    }

    pub fn lookup_object(&self, id: ObjID) -> Option<ObjectInfo> {
        let mut b = [];
        object_store::read_exact(id.raw(), &mut b, 0).ok()?;
        Some(ObjectInfo::new(id))
    }

    pub async fn sync(
        &self,
        rq: &Arc<QueueSender<RequestFromPager, CompletionToPager>>,
        id: ObjID,
    ) {
        tracing::debug!("sync: {:?}", id);
        let po = {
            let mut inner = self.inner.lock().unwrap();
            inner.get_per_object(id).clone()
        };
        let _ = po.sync(rq).await.inspect_err(|e| {
            tracing::warn!("sync failed for {}: {}", id, e);
        });
        object_store::advance_epoch().unwrap();
    }
}
