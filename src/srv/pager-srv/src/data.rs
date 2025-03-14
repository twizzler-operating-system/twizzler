use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use itertools::Itertools;
use miette::Result;
use object_store::{objid_to_ino, PageRequest};
use secgate::util::{Descriptor, HandleMgr};
use twizzler::object::ObjID;
use twizzler_abi::pager::{ObjectInfo, ObjectRange, PhysRange};

use crate::{
    disk::DiskPageRequest,
    handle::PagerClient,
    helpers::{page_in, page_out, page_out_many, PAGE},
    PagerContext,
};

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

    pub async fn sync(&self, ctx: &'static PagerContext) -> Result<()> {
        tracing::debug!("sync: {}", self.id);
        let (pages, mpages) = {
            let inner = self.inner.lock().unwrap();
            let total = inner.page_map.len() + inner.meta_page_map.len();
            tracing::debug!("syncing {}: {} pages", self.id, total);
            let mut pages = inner.pages().map(|p| (p.0, vec![p.1])).collect::<Vec<_>>();
            pages.sort_by_key(|p| p.0);
            let pages = pages
                .into_iter()
                .coalesce(|mut x, y| {
                    if x.0.end == y.0.start {
                        x.1.extend(y.1);
                        Ok((ObjectRange::new(x.0.start, y.0.end), x.1))
                    } else {
                        Err((x, y))
                    }
                })
                .collect::<Vec<_>>();
            let mut mpages = inner.meta_pages().collect::<Vec<_>>();
            mpages.sort_by_key(|p| p.0);
            (pages, mpages)
        };

        let reqs = pages
            .into_iter()
            .map(|p| {
                let start_page = p.0.pages().next().unwrap();
                let nr_pages = p.1.len();
                assert_eq!(nr_pages, p.0.pages().count());
                PageRequest::new(
                    ctx.disk
                        .new_paging_request::<DiskPageRequest>(p.1.into_iter().map(|pd| pd.paddr)),
                    start_page as i64,
                    nr_pages as u32,
                )
            })
            .collect_vec();

        page_out_many(ctx, self.id, reqs).await?;

        for p in mpages {
            let phys_range = PhysRange::new(p.1.paddr, p.1.paddr + PAGE);
            tracing::trace!("sync: meta page: {:?} {:?}", p, phys_range);
            page_out(ctx, self.id, p.0, phys_range, true).await?;
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

pub struct PagerData {
    inner: Arc<Mutex<PagerDataInner>>,
}

#[allow(dead_code)]
impl PagerData {
    pub fn avail_mem(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner
            .memory
            .regions
            .iter()
            .fold(0, |acc, item| acc + item.avail())
    }

    pub fn alloc_page(&self) -> Option<u64> {
        self.inner.lock().unwrap().get_next_available_page()
    }
}

pub struct PagerDataInner {
    memory: Memory,
    pub per_obj: HashMap<ObjID, PerObject>,
    pub handles: HandleMgr<PagerClient>,
}

struct Region {
    unused_start: u64,
    end: u64,
    stack: Vec<u64>,
}

#[allow(dead_code)]
impl Region {
    pub fn avail(&self) -> usize {
        let unused = self.end - self.unused_start;
        unused as usize + self.stack.len() * PAGE as usize
    }

    pub fn new(range: PhysRange) -> Self {
        Self {
            unused_start: range.start,
            end: range.end,
            stack: Vec::new(),
        }
    }
    pub fn get_page(&mut self) -> Option<u64> {
        self.stack.pop().or_else(|| {
            if self.unused_start == self.end {
                None
            } else {
                let next = self.unused_start;
                self.unused_start += PAGE;
                Some(next)
            }
        })
    }

    pub fn release_page(&mut self, page: u64) {
        if self.unused_start - PAGE == page {
            self.unused_start -= PAGE;
        } else {
            self.stack.push(page);
        }
    }
}

#[derive(Default)]
struct Memory {
    regions: Vec<Region>,
}

impl Memory {
    pub fn push(&mut self, region: Region) {
        self.regions.push(region);
    }

    pub fn get_page(&mut self) -> Option<u64> {
        for region in &mut self.regions {
            if let Some(page) = region.get_page() {
                return Some(page);
            }
        }
        None
    }
}

impl PagerDataInner {
    /// Create a new PagerDataInner instance
    /// Initializes the data structure for managing page allocations and replacements.
    pub fn new() -> Self {
        tracing::trace!("initializing PagerDataInner");
        PagerDataInner {
            per_obj: HashMap::with_capacity(0),
            memory: Memory::default(),
            handles: HandleMgr::new(None),
        }
    }

    /// Get the next available page number and mark it as used.
    /// Returns the page number if available, or `None` if all pages are used.
    fn get_next_available_page(&mut self) -> Option<u64> {
        self.memory.get_page()
    }

    /// Get a memory page for allocation.
    /// Triggers page replacement if all pages are used.
    fn get_mem_page(&mut self) -> u64 {
        tracing::trace!("attempting to get memory page");
        self.get_next_available_page().expect("no available pages")
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

    /// Initialize the starting memory range for the pager.
    pub fn init_range(&self, range: PhysRange) {
        self.inner.lock().unwrap().memory.push(Region::new(range));
    }

    /// Allocate a memory page and associate it with an object and range.
    /// Page in the data from disk
    /// Returns the physical range corresponding to the allocated page.
    pub async fn fill_mem_page(
        &self,
        ctx: &'static PagerContext,
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
            let phys_range = PhysRange::new(page, page + PAGE);
            let po = inner.get_per_object_mut(id);
            po.track(obj_range, phys_range);
            phys_range
        };
        page_in(ctx, id, obj_range, phys_range, false).await?;
        tracing::debug!("memory page allocated successfully: {:?}", phys_range);
        return Ok(phys_range);
    }

    pub fn lookup_object(&self, ctx: &PagerContext, id: ObjID) -> Option<ObjectInfo> {
        let mut b = [];
        if objid_to_ino(id.raw()).is_some() {
            ctx.paged_ostore.find_external(id.raw()).ok()?;
            return Some(ObjectInfo::new(id));
        }
        ctx.paged_ostore.read_object(id.raw(), 0, &mut b).ok()?;
        Some(ObjectInfo::new(id))
    }

    pub async fn sync(&self, ctx: &'static PagerContext, id: ObjID) {
        let po = {
            let mut inner = self.inner.lock().unwrap();
            inner.get_per_object(id).clone()
        };
        let _ = po.sync(ctx).await.inspect_err(|e| {
            tracing::warn!("sync failed for {}: {}", id, e);
        });
        ctx.paged_ostore.flush().unwrap();
    }

    pub fn with_handle<R>(
        &self,
        comp: ObjID,
        ds: Descriptor,
        f: impl FnOnce(&PagerClient) -> R,
    ) -> Option<R> {
        let inner = self.inner.lock().unwrap();
        Some(f(inner.handles.lookup(comp, ds)?))
    }

    pub fn with_handle_mut<R>(
        &self,
        comp: ObjID,
        ds: Descriptor,
        f: impl FnOnce(&mut PagerClient) -> R,
    ) -> Option<R> {
        let mut inner = self.inner.lock().unwrap();
        Some(f(inner.handles.lookup_mut(comp, ds)?))
    }

    pub fn new_handle(&self, comp: ObjID) -> Option<Descriptor> {
        let mut inner = self.inner.lock().unwrap();
        inner.handles.insert(comp, PagerClient::new()?)
    }

    pub fn drop_handle(&self, comp: ObjID, ds: Descriptor) {
        let mut inner = self.inner.lock().unwrap();
        inner.handles.remove(comp, ds);
    }
}
