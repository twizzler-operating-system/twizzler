use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    task::Waker,
};

use itertools::Itertools;
use object_store::{objid_to_ino, PageRequest, PagedObjectStore, PagedPhysMem};
use secgate::util::{Descriptor, HandleMgr};
use stable_vec::StableVec;
use twizzler::object::ObjID;
use twizzler_abi::{
    object::{Protections, MAX_SIZE},
    pager::{
        CompletionToKernel, KernelCompletionData, KernelCompletionFlags, ObjectEvictFlags,
        ObjectEvictInfo, ObjectInfo, ObjectRange, PhysRange,
    },
    syscall::{BackingType, LifetimeType},
};
use twizzler_rt_abi::{
    error::{ArgumentError, ResourceError},
    Result,
};

use crate::{
    handle::PagerClient,
    helpers::{page_in, page_in_many, page_out_many, PAGE},
    iotop::PagerIOTop,
    iotop::PagerIotopData,
    PagerContext,
};

type PageNum = u64;

#[derive(Debug, Clone, Copy, Default)]
pub struct PerPageData {
    paddr: u64,
    version: u64,
}

#[derive(Default)]
pub struct PerObjectInner {
    #[allow(dead_code)]
    id: ObjID,
    sync_map: HashMap<PageNum, PerPageData>,
    syncing: bool,
}

impl PerObjectInner {
    pub fn track(&mut self, obj_range: ObjectRange, phys_range: PhysRange, version: u64) {
        assert_eq!(obj_range.len(), phys_range.len());
        for (op, pp) in obj_range.pages().zip(phys_range.pages()) {
            let entry = self.sync_map.entry(op).or_default();
            if entry.version <= version {
                entry.paddr = pp * PAGE;
            }
        }
    }

    fn drain_pending_syncs(&mut self) -> impl Iterator<Item = (ObjectRange, PhysRange, u64)> + '_ {
        self.sync_map.drain().map(|(obj_page, pp)| {
            (
                ObjectRange::new(obj_page * PAGE, (obj_page + 1) * PAGE),
                PhysRange::new(pp.paddr, pp.paddr + PAGE),
                pp.version,
            )
        })
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
    inner: Arc<(
        async_condvar_fair::Condvar,
        async_lock::Mutex<PerObjectInner>,
    )>,
}

impl PerObject {
    async fn do_sync_region(
        &self,
        ctx: &'static PagerContext,
        info: &ObjectEvictInfo,
    ) -> (usize, CompletionToKernel) {
        let pages = {
            let mut inner = self.inner.1.lock().await;
            inner.track(info.range, info.phys, info.version);
            while inner.syncing {
                self.inner.0.wait_no_relock(inner).await;
                inner = self.inner.1.lock().await;
            }
            inner.syncing = true;
            let mut pages = inner
                .drain_pending_syncs()
                .map(|p| (p.0, vec![PagedPhysMem::new(p.1)]))
                .collect::<Vec<_>>();
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
            tracing::debug!("drained {:?}", pages);
            pages
        };
        let reqs = pages
            .into_iter()
            .map(|p| {
                let mut start_page = p.0.pages().next().unwrap();
                if p.0.start == (MAX_SIZE as u64) - PAGE {
                    start_page = 0;
                }
                let nr_pages = p.1.len();
                assert_eq!(nr_pages, p.0.pages().count());
                PageRequest::new_from_list(p.1, start_page as i64, nr_pages as u32)
            })
            .collect_vec();
        let count = match page_out_many(ctx, self.id, reqs).await {
            Err(e) => {
                let mut inner = self.inner.1.lock().await;
                inner.syncing = false;
                self.inner.0.notify_all();
                return (
                    0,
                    CompletionToKernel::new(
                        KernelCompletionData::Error(e.into()),
                        KernelCompletionFlags::DONE,
                    ),
                );
            }
            Ok(count) => count,
        };

        let mut inner = self.inner.1.lock().await;
        inner.syncing = false;
        self.inner.0.notify_all();

        (
            count,
            CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::DONE),
        )
    }

    pub async fn sync_region(
        &self,
        ctx: &'static PagerContext,
        info: &ObjectEvictInfo,
    ) -> (usize, CompletionToKernel) {
        tracing::debug!("push pending sync: {:?}", info);
        if info.flags.contains(ObjectEvictFlags::FENCE) {
            self.do_sync_region(ctx, info).await
        } else {
            let mut inner = self.inner.1.lock().await;
            inner.track(info.range, info.phys, info.version);
            (
                0,
                CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::empty()),
            )
        }
    }

    pub fn new(id: ObjID) -> Self {
        Self {
            id,
            inner: Arc::new((
                async_condvar_fair::Condvar::new(),
                async_lock::Mutex::new(PerObjectInner::new(id)),
            )),
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

    pub fn free_page(&self, page: u64) {
        self.inner.lock().unwrap().free_page(page);
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
    pub iotop: PagerIOTop
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

    pub fn release_page(&mut self, page: u64) -> bool {
        if self.unused_start - PAGE == page {
            self.unused_start -= PAGE;
        } else {
            self.stack.push(page);
        }
        true
    }

    pub fn try_release_page(&mut self, page: u64) -> bool {
        if self.unused_start - PAGE == page {
            self.unused_start -= PAGE;
            true
        } else {
            false
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
        let i = 0;
        while i < self.regions.len() {
            if let Some(page) = self.regions[i].get_page() {
                return Some(page);
            }
            self.regions.swap_remove(i);
        }
        None
    }

    pub fn free_page(&mut self, page: u64) {
        for region in &mut self.regions {
            if region.try_release_page(page) {
                return;
            }
        }

        for region in &mut self.regions {
            if region.release_page(page) {
                return;
            }
        }
    }

    pub fn available_memory(&self) -> usize {
        self.regions.iter().map(|r| r.avail()).sum()
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
            iotop: PagerIOTop::new(),
        }
    }

    /// Get the next available page number and mark it as used.
    /// Returns the page number if available, or `None` if all pages are used.
    fn get_next_available_page(&mut self) -> Option<u64> {
        self.memory.get_page()
    }

    fn free_page(&mut self, page: u64) {
        self.memory.free_page(page);
    }

    pub fn get_per_object(&mut self, id: ObjID) -> &PerObject {
        self.per_obj.entry(id).or_insert_with(|| PerObject::new(id))
    }


    pub fn record_io(&mut self, obj_id: ObjID, read_pages: usize, written_pages: usize) {
        self.iotop.record_io(obj_id, read_pages, written_pages);
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
        tracing::debug!("add memory range: {} pages", range.pages().count());
        inner.memory.push(Region::new(range));
        for item in inner.waiters.values() {
            if let Some(waker) = item {
                waker.wake_by_ref();
            }
        }
    }

    async fn do_fill_pages(
        &self,
        ctx: &'static PagerContext,
        id: ObjID,
        obj_range: ObjectRange,
        _partial: bool,
    ) -> Result<Vec<PagedPhysMem>> {
        let current_mem_pages = ctx.data.avail_mem() / PAGE as usize;
        let max_pages = (current_mem_pages / 2).min(4096 * 128);
        tracing::trace!(
            "req: {}, cur: {} ({})",
            obj_range.pages().count(),
            current_mem_pages,
            current_mem_pages / 2
        );

        let start_page = obj_range.pages().next().unwrap();
        let nr_pages = obj_range.pages().count().min(max_pages).max(1);
        let reqs = vec![PageRequest::new(start_page as i64, nr_pages as u32)];
        let (mut reqs, count) = page_in_many(ctx, id, reqs).await?;
        if count == 0 {
            // TODO: free pages in incomplete requests.
            todo!();
        }

        Ok(reqs.pop().unwrap().into_list())
    }
    /// Allocate a memory page and associate it with an object and range.
    /// Page in the data from disk
    /// Returns the physical range corresponding to the allocated page.
    pub async fn fill_mem_pages_partial(
        &self,
        ctx: &'static PagerContext,
        id: ObjID,
        obj_range: ObjectRange,
    ) -> Result<Vec<PagedPhysMem>> {
        // TODO: will need to check if the range contains this, not just starts here.
        if obj_range.start == (MAX_SIZE as u64) - PAGE {
            return Ok(self
                .fill_mem_pages_legacy(ctx, id, obj_range)
                .await?
                .into_iter()
                .map(|p| PagedPhysMem::new(p.1).completed())
                .collect());
        }

        let pages = self.do_fill_pages(ctx, id, obj_range, true).await?;

        {
            ctx.data.record_io_read(id, obj_range.len() / PAGE as usize);
        }

        Ok(pages)
    }

    /// Allocate a memory page and associate it with an object and range.
    /// Page in the data from disk
    /// Returns the physical range corresponding to the allocated page.
    pub async fn fill_mem_pages_legacy(
        &self,
        ctx: &'static PagerContext,
        id: ObjID,
        obj_range: ObjectRange,
    ) -> Result<Vec<(ObjectRange, PhysRange)>> {
        let mut r = Vec::new();
        for i in 0..(obj_range.pages().count() as u64) {
            let range = ObjectRange::new(
                obj_range.start + i * PAGE,
                obj_range.start + i * PAGE + PAGE,
            );
            r.push((range, self.fill_mem_page(ctx, id, range).await?));
        }
        Ok(r)
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
                    tracing::warn!("out of memory -- task waiting");
                    mw.await
                }
            };
            let phys_range = PhysRange::new(page, page + PAGE);
            phys_range
        };
        page_in(ctx, id, obj_range, phys_range).await?;
        tracing::debug!("memory page allocated successfully: {:?}", phys_range);

        {
            ctx.data.record_io_read(id, obj_range.len() / PAGE as usize);
        }

        return Ok(phys_range);
    }

    pub async fn lookup_object(&self, ctx: &'static PagerContext, id: ObjID) -> Result<ObjectInfo> {
        let mut b = [];
        if objid_to_ino(id.raw()).is_some() {
            blocking::unblock(move || ctx.paged_ostore(None)?.find_external(id.raw())).await?;
            return Ok(ObjectInfo::new(
                LifetimeType::Persistent,
                BackingType::Normal,
                0.into(),
                0,
                Protections::empty(),
            ));
        }
        blocking::unblock(move || ctx.paged_ostore(None)?.read_object(id.raw(), 0, &mut b)).await?;
        Ok(ObjectInfo::new(
            LifetimeType::Persistent,
            BackingType::Normal,
            0.into(),
            0,
            Protections::empty(),
        ))
    }

    pub async fn sync_region(
        &self,
        ctx: &'static PagerContext,
        info: &ObjectEvictInfo,
    ) -> CompletionToKernel {
        let po = {
            let mut inner = self.inner.lock().unwrap();
            inner.get_per_object(info.obj_id).clone()
        };

        let (count, compl) = po.sync_region(ctx, info).await;
        if count > 0 {
            ctx.data.record_io_write(info.obj_id, count);
        }
        compl
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

    pub fn record_io_read(&self, obj_id: ObjID, read_pages: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.iotop.record_io(obj_id, read_pages, 0);
    }

    pub fn record_io_write(&self, obj_id: ObjID, written_pages: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.iotop.record_io(obj_id, 0, written_pages);
    }

    pub fn get_object_pager_data(&self, obj_id: ObjID) -> Option<PagerIotopData> {
        let mut inner = self.inner.lock().unwrap();
        inner.iotop.get_object_data(obj_id)
    }

    pub fn get_nth_iotop_object_id(&self, n: usize) -> Option<ObjID> {
        let mut inner = self.inner.lock().unwrap();
        inner.iotop.get_nth_object_id(n)
    }


}
