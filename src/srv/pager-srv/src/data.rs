use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    task::Waker,
};

use itertools::Itertools;
use object_store::{objid_to_ino, PageRequest};
use secgate::util::{Descriptor, HandleMgr};
use stable_vec::StableVec;
use twizzler::object::ObjID;
use twizzler_abi::{
    pager::{ObjectInfo, ObjectRange, PhysRange},
    syscall::{BackingType, LifetimeType},
};
use twizzler_rt_abi::{
    error::{ArgumentError, ResourceError},
    Result,
};

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
            page_out(ctx, self.id, p.0, phys_range).await?;
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

    pub fn try_alloc_page(&self) -> core::result::Result<u64, MemoryWaiter> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(page) = inner.get_next_available_page() {
            return Ok(page);
        }
        let pos = inner.waiters.push(None);
        tracing::debug!("memory allocation failed");
        drop(inner);
        Err(MemoryWaiter::new(pos, self.inner.clone()))
    }
}

pub struct PagerDataInner {
    memory: Memory,
    waiters: StableVec<Option<Waker>>,
    pub per_obj: HashMap<ObjID, PerObject>,
    pub handles: HandleMgr<PagerClient>,
}

pub struct MemoryWaiter {
    pos: usize,
    inner: Arc<Mutex<PagerDataInner>>,
}

impl MemoryWaiter {
    pub fn new(pos: usize, inner: Arc<Mutex<PagerDataInner>>) -> Self {
        Self { pos, inner }
    }
}

impl Future for MemoryWaiter {
    type Output = u64;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut inner = self.inner.lock().unwrap();

        if let Some(page) = inner.get_next_available_page() {
            std::task::Poll::Ready(page)
        } else {
            inner.waiters[self.pos] = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}

impl Drop for MemoryWaiter {
    fn drop(&mut self) {
        self.inner.lock().unwrap().waiters.remove(self.pos);
    }
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
            waiters: StableVec::new(),
        }
    }

    /// Get the next available page number and mark it as used.
    /// Returns the page number if available, or `None` if all pages are used.
    fn get_next_available_page(&mut self) -> Option<u64> {
        self.memory.get_page()
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
    pub fn add_memory_range(&self, range: PhysRange) {
        let mut inner = self.inner.lock().unwrap();
        inner.memory.push(Region::new(range));
        for item in inner.waiters.values() {
            if let Some(waker) = item {
                waker.wake_by_ref();
            }
        }
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
            let page = match self.try_alloc_page() {
                Ok(page) => page,
                Err(mw) => {
                    tracing::debug!("out of memory -- task waiting");
                    mw.await
                }
            };
            let phys_range = PhysRange::new(page, page + PAGE);
            self.inner
                .lock()
                .unwrap()
                .get_per_object_mut(id)
                .track(obj_range, phys_range);
            phys_range
        };
        page_in(ctx, id, obj_range, phys_range).await?;
        tracing::debug!("memory page allocated successfully: {:?}", phys_range);
        return Ok(phys_range);
    }

    pub fn lookup_object(&self, ctx: &PagerContext, id: ObjID) -> Result<ObjectInfo> {
        let mut b = [];
        if objid_to_ino(id.raw()).is_some() {
            ctx.paged_ostore.find_external(id.raw())?;
            return Ok(ObjectInfo::new(
                LifetimeType::Persistent,
                BackingType::Normal,
                0.into(),
                0,
            ));
        }
        ctx.paged_ostore.read_object(id.raw(), 0, &mut b)?;
        Ok(ObjectInfo::new(
            LifetimeType::Persistent,
            BackingType::Normal,
            0.into(),
            0,
        ))
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
    ) -> Result<R> {
        let inner = self.inner.lock().unwrap();
        Ok(f(inner
            .handles
            .lookup(comp, ds)
            .ok_or(ArgumentError::BadHandle)?))
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

    pub fn new_handle(&self, comp: ObjID) -> Result<Descriptor> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .handles
            .insert(comp, PagerClient::new()?)
            .ok_or(ResourceError::OutOfResources.into())
    }

    pub fn drop_handle(&self, comp: ObjID, ds: Descriptor) {
        let mut inner = self.inner.lock().unwrap();
        inner.handles.remove(comp, ds);
    }
}
