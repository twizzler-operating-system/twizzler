use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Instant,
};

use itertools::Itertools;
use object_store::{objid_to_ino, PageRequest, PagedObjectStore, PagedPhysMem, INLINE_LEN};
use secgate::util::{Descriptor, HandleMgr};
use twizzler::{
    error::ObjectError,
    object::{ObjID, ObjectHandle},
};
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
    helpers::{page_in, page_in_many, page_out_many, EXTERNAL_META, PAGE},
    stats::RecentStats,
    PagerContext,
};

#[derive(Default)]
pub struct PerObjectInner {
    #[allow(dead_code)]
    id: ObjID,
    sync_map: Vec<(ObjectRange, PhysRange, u64, u128)>,
    syncing: bool,
}

impl PerObjectInner {
    pub fn track(
        &mut self,
        obj_range: ObjectRange,
        phys_range: PhysRange,
        version: u64,
        uniq_id: u128,
    ) {
        self.sync_map
            .push((obj_range, phys_range, version, uniq_id));
        /*
        for (op, pp) in obj_range.pages().zip(phys_range.pages()) {
            let entry = self.sync_map.entry(op).or_default();
            if entry.version <= version {
                entry.paddr = pp * PAGE;
            }
        }
        */
    }

    fn drain_pending_syncs(
        &mut self,
        version: u64,
        uniq_id: u128,
    ) -> impl Iterator<Item = (ObjectRange, PhysRange, u64, u128)> + '_ {
        self.sync_map
            .extract_if(.., move |p| p.2 <= version && p.3 == uniq_id)
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
    inner: Arc<(Condvar, Mutex<PerObjectInner>)>,
}

impl PerObject {
    fn do_sync_region(
        &self,
        ctx: &'static PagerContext,
        info: &ObjectEvictInfo,
        work: &crate::watchdog::Work,
    ) -> (usize, CompletionToKernel) {
        let start = Instant::now();
        let pages = {
            work.phase("sync:lock");
            let mut inner = self.inner.1.lock().unwrap();
            inner.track(info.range, info.phys, info.version, info.uniq_id.raw());
            while inner.syncing {
                tracing::info!("waiting for syncing {:?}", info);
                work.phase("sync:wait-for-other-sync");
                let (guard, timeout) = self
                    .inner
                    .0
                    .wait_timeout(inner, crate::threads::WAKE_FALLBACK)
                    .unwrap();
                if timeout.timed_out() {
                    crate::threads::WAKE_WATCH.fallback(!guard.syncing, "object sync fence");
                }
                inner = guard;
            }
            work.phase("sync:collect-pages");
            inner.syncing = true;
            let mut pages = inner
                .drain_pending_syncs(info.version, info.uniq_id.raw())
                .map(|p| {
                    (
                        p.0,
                        object_store::Vec::<_, INLINE_LEN>::from_slice(&[PagedPhysMem::new(p.1)]),
                    )
                })
                .collect::<Vec<_>>();
            // TODO: sort with -version as well
            pages.sort_by_key(|p| p.0);
            pages.dedup_by_key(|p| p.0);
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
                .collect::<std::vec::Vec<_>>();
            pages
        };
        let pages_done = Instant::now();
        let mut page_count = 0;
        let mut set_len = None;
        let mut reqs = pages
            .into_iter()
            .filter_map(|p| {
                if let Some(mut start_page) = p.0.pages().next() {
                    if p.0.start == (MAX_SIZE as u64) - PAGE {
                        start_page = 0;
                        if objid_to_ino(self.id.raw()).is_some() {
                            // The kernel read `MEXT_SIZED` out of this same meta page before
                            // sending it. This used to be a `read_physical_pages` here, which is a
                            // `CopyUserPhys` back into the kernel -- serviced by a single kernel
                            // thread, and taken synchronously while holding a kernel request open,
                            // so every worker needing it serialized behind one another.
                            let len = info.len;
                            tracing::trace!(
                                "meta page for external file, len: {}, range: {:?}",
                                len,
                                p.1[0].range
                            );
                            set_len = Some(len);
                            ctx.paged_ostore(None)
                                .unwrap()
                                .set_len(self.id.raw(), len)
                                .unwrap();
                            return None;
                        }
                    }
                    let nr_pages = p.1.iter().fold(0, |acc, x| acc + x.nr_pages());
                    page_count += nr_pages;
                    assert_eq!(nr_pages, p.0.page_count());
                    Some(PageRequest::new_from_list(
                        p.1,
                        start_page as i64,
                        nr_pages as u32,
                    ))
                } else {
                    None
                }
            })
            .collect::<std::vec::Vec<_>>();
        if page_count >= 1024 * 16 {
            tracing::info!(
                "pager starting large sync for {}: {}MB: {}",
                self.id,
                (page_count as u64 * PAGE) / (1024 * 1024),
                reqs.len(),
            );
        }
        let reqs_done = Instant::now();
        work.phase("sync:page-out");
        let count = match page_out_many(ctx, self.id, reqs.as_mut_slice()) {
            Err(e) => {
                let mut inner = self.inner.1.lock().unwrap();
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
        if let Some(len) = set_len {
            ctx.paged_ostore(None)
                .unwrap()
                .set_len(self.id.raw(), len)
                .unwrap();
        }
        let io_done = Instant::now();
        if page_count >= 1024 * 16 {
            tracing::info!(
                "pager finished large sync for {}: {}ms, {}ms",
                self.id,
                (reqs_done - pages_done).as_millis(),
                (io_done - reqs_done).as_millis(),
            );
        }
        work.phase("sync:relock");
        let mut inner = self.inner.1.lock().unwrap();
        inner.syncing = false;
        self.inner.0.notify_one();
        let done = Instant::now();

        tracing::debug!(
            "==> {}ms {}ms {}ms {}ms",
            (pages_done - start).as_millis(),
            (reqs_done - pages_done).as_millis(),
            (io_done - pages_done).as_millis(),
            (done - io_done).as_millis()
        );
        (
            count,
            CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::DONE),
        )
    }

    pub fn sync_region(
        &self,
        ctx: &'static PagerContext,
        info: &ObjectEvictInfo,
        work: &crate::watchdog::Work,
    ) -> (usize, CompletionToKernel) {
        tracing::debug!("push pending sync: {:?}", info);
        if info.flags.contains(ObjectEvictFlags::FENCE) {
            self.do_sync_region(ctx, info, work)
        } else {
            work.phase("track:lock");
            let mut inner = self.inner.1.lock().unwrap();
            inner.track(info.range, info.phys, info.version, info.uniq_id.raw());
            (
                0,
                CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::DONE),
            )
        }
    }

    pub fn new(id: ObjID) -> Self {
        Self {
            id,
            inner: Arc::new((Condvar::new(), Mutex::new(PerObjectInner::new(id)))),
        }
    }
}

pub struct PagerData {
    inner: Arc<Mutex<PagerDataInner>>,
    /// How many threads are parked in [`MemoryWaiter`], so the hot free path can skip the notify
    /// syscall when nobody is waiting.
    mem_waiters: Arc<AtomicUsize>,
    /// Signalled whenever pages are returned to the pool, for threads parked in [`MemoryWaiter`].
    ///
    /// Replaces a `StableVec<Option<Waker>>` of registered wakers: with blocking allocators every
    /// waiter is a thread that can recheck the pool for itself, so one condvar on the pool's own
    /// mutex says everything the per-waiter slots did.
    mem_avail: Arc<Condvar>,
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
        let page = self.inner.lock().unwrap().get_next_available_page();
        // PRPTRACE (see nvme/dma.rs): catch garbage leaving the pool.
        if let Some(p) = page {
            if p & 0xFFF != 0 {
                tracing::error!("PRPTRACE pool returned misaligned page {:x}", p);
            }
        }
        page
    }

    pub fn free_page(&self, page: u64) {
        // PRPTRACE (see nvme/dma.rs): catch garbage entering the pool.
        if page & 0xFFF != 0 {
            tracing::error!("PRPTRACE misaligned page freed to pool: {:x}", page);
        }
        self.inner.lock().unwrap().free_page(page);
        // A page returned here satisfies a waiter just as a kernel donation does, but only
        // `add_memory_range` ever signalled -- so a thread parked for memory could not be woken by
        // another thread freeing some, and waited for the kernel instead. That gap predates the
        // blocking rewrite (the waker list had the same one); this is where it closes.
        //
        // Gated on the count so the common case stays a relaxed load: waiters only exist while the
        // pool is empty, which is already the slow path.
        if self.mem_waiters.load(Ordering::Relaxed) > 0 {
            self.mem_avail.notify_all();
        }
    }

    pub fn try_alloc_page(&self) -> core::result::Result<u64, MemoryWaiter> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(page) = inner.get_next_available_page() {
            return Ok(page);
        }
        tracing::debug!("memory allocation failed");
        drop(inner);
        Err(MemoryWaiter::new(
            self.inner.clone(),
            self.mem_avail.clone(),
            self.mem_waiters.clone(),
        ))
    }

    pub fn try_alloc_pages(
        &self,
        start: i64,
        len: u32,
    ) -> core::result::Result<(u64, u32), MemoryWaiter> {
        let mut inner = self.inner.lock().unwrap();

        tracing::trace!(
            "try_alloc_pages: start = {}, len = {}, avail = {}",
            start,
            len,
            inner.memory.available_memory()
        );
        let align = start as usize * PAGE as usize;
        if align.is_multiple_of(1024 * 1024 * 2) && len >= 512 {
            tracing::trace!("trying full aligned alloc");
            if let Some(page) = inner.get_next_available_pages(1024 * 1024 * 2, 512 * PAGE as usize)
            {
                tracing::trace!(
                    "allocated {} aligned pages at {} (align: {})",
                    512,
                    page,
                    align
                );
                return Ok((page, 512));
            }
        }

        let rem = 1024 * 1024 * 2 - align % (1024 * 1024 * 2);
        if len > 1 && rem > 0x1000 && align > 0 {
            tracing::trace!(
                "requesting {} pages with alignment {}, rem = {}",
                rem as u64 / PAGE,
                align,
                rem
            );
            let thislen = rem.min(len as usize * PAGE as usize);
            let thiscount = thislen / PAGE as usize;
            assert!(rem.is_multiple_of(0x1000));
            if let Some(page) = inner.get_next_available_pages(0x1000, thislen) {
                tracing::trace!(
                    "allocated {} pages at {} (align: {})",
                    thiscount,
                    page,
                    align
                );
                return Ok((page, thiscount as u32));
            }
        }
        if len > 1 {
            if let Some(page) = inner.get_next_available_pages(0x1000, len as usize * PAGE as usize)
            {
                tracing::trace!("allocated {} pages at {} (align: {})", len, page, align);
                return Ok((page, len));
            }
        }
        if let Some(page) = inner.get_next_available_page() {
            // PRPTRACE (see nvme/dma.rs): catch garbage leaving the pool on the fill path.
            if page & 0xFFF != 0 {
                tracing::error!("PRPTRACE pool returned misaligned page {:x} (single)", page);
            }
            return Ok((page, 1));
        }
        tracing::debug!("memory allocation failed");
        drop(inner);
        Err(MemoryWaiter::new(
            self.inner.clone(),
            self.mem_avail.clone(),
            self.mem_waiters.clone(),
        ))
    }

    pub fn print_stats(&self) {
        let inner = self.inner.lock().unwrap();
        inner.print_stats();
    }

    pub fn reset_stats(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.reset_stats();
    }
}

pub struct PagerDataInner {
    memory: Memory,
    pub per_obj: HashMap<ObjID, PerObject>,
    pub handles: HandleMgr<PagerClient>,
    pub recent_stats: RecentStats,
}

/// What an allocator hands back when the pool is empty: block on this until it is not.
///
/// The condvar is signalled by [`PagerData::add_memory_range`], i.e. when the kernel donates
/// memory back. Rechecked in a loop rather than trusted, since several waiters wake together and
/// only some of them will find a page.
pub struct MemoryWaiter {
    inner: Arc<Mutex<PagerDataInner>>,
    avail: Arc<Condvar>,
    waiters: Arc<AtomicUsize>,
}

impl MemoryWaiter {
    pub fn new(
        inner: Arc<Mutex<PagerDataInner>>,
        avail: Arc<Condvar>,
        waiters: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            inner,
            avail,
            waiters,
        }
    }

    pub fn wait(self) -> u64 {
        self.waiters.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        let page = loop {
            if let Some(page) = inner.get_next_available_page() {
                break page;
            }
            let (guard, timeout) = self
                .avail
                .wait_timeout(inner, crate::threads::WAKE_FALLBACK)
                .unwrap();
            if timeout.timed_out() {
                // Peek rather than take: reporting has to leave the page for the loop to claim.
                let owed = guard.memory.available_memory() > 0;
                crate::threads::WAKE_WATCH.fallback(owed, "pager memory pool");
            }
            inner = guard;
        };
        self.waiters.fetch_sub(1, Ordering::Relaxed);
        page
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

    pub fn get_pages(&mut self, align: usize, len: usize) -> Option<u64> {
        if self.unused_start.is_multiple_of(align as u64)
            && self.unused_start + len as u64 <= self.end
        {
            let next = self.unused_start;
            self.unused_start += len as u64;
            return Some(next);
        }

        if self.unused_start + align as u64 + len as u64 <= self.end {
            let next = (self.unused_start + align as u64 - 1) & !(align as u64 - 1);
            while self.unused_start + PAGE <= next {
                self.stack.push(self.unused_start);
                self.unused_start += PAGE;
            }
            self.unused_start = next + len as u64;
            self.stack.sort_unstable_by(|a, b| b.cmp(a));
            return Some(next);
        }

        None
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
        while !self.regions.is_empty() {
            if let Some(page) = self.regions[0].get_page() {
                return Some(page);
            }
            self.regions.swap_remove(0);
        }
        None
    }

    pub fn get_pages(&mut self, align: usize, len: usize) -> Option<u64> {
        let mut i = 0;
        while i < self.regions.len() {
            if let Some(page) = self.regions[i].get_pages(align, len) {
                return Some(page);
            }
            if self.regions[i].avail() == 0 {
                self.regions.swap_remove(i);
            } else {
                i += 1;
            }
        }
        None
    }

    pub fn try_add_memory_range(&mut self, range: PhysRange) -> bool {
        for region in &mut self.regions {
            if region.unused_start == range.end {
                region.unused_start = range.start;
                return true;
            } else if region.end == range.start {
                region.end = range.end;
                return true;
            }
        }
        false
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
            recent_stats: RecentStats::new(),
        }
    }

    /// Get the next available page number and mark it as used.
    /// Returns the page number if available, or `None` if all pages are used.
    fn get_next_available_page(&mut self) -> Option<u64> {
        self.memory.get_page()
    }

    fn get_next_available_pages(&mut self, align: usize, len: usize) -> Option<u64> {
        self.memory.get_pages(align, len)
    }

    fn free_page(&mut self, page: u64) {
        self.memory.free_page(page);
    }

    pub fn get_per_object(&mut self, id: ObjID) -> &PerObject {
        self.per_obj.entry(id).or_insert_with(|| PerObject::new(id))
    }

    pub fn print_stats(&self) {
        let dt = self.recent_stats.dt();
        let mut total_read_kbps = 0.;
        let mut total_write_kbps = 0.;
        let mut count = 0;
        for (id, stats) in self.recent_stats.recorded_stats() {
            let read = crate::stats::pages_to_kbytes_per_sec(stats.pages_read, dt);
            let write = crate::stats::pages_to_kbytes_per_sec(stats.pages_written, dt);
            tracing::debug!(
                "{}: read {:3.3} KB/s ({:8.8} pages), write {:3.3} KB/s ({:8.8} pages)",
                id,
                read,
                stats.pages_read,
                write,
                stats.pages_written
            );

            count += 1;
            total_read_kbps += read;
            total_write_kbps += write;
        }
        if true || self.recent_stats.had_activity() {
            tracing::info!(
                "PAGER STATS: Available memory: {:10.10} KB, r {:3.3} KB/s w {:3.3} KB/s c {:2.2} (dt: {:2.2}s)",
                self.memory.available_memory() / 1024,
                total_read_kbps,total_write_kbps,
                count,
                dt.as_secs_f32(),
            );
        }
    }

    pub fn reset_stats(&mut self) {
        self.recent_stats.reset();
    }
}

impl PagerData {
    /// Create a new PagerData instance.
    /// Wraps PagerDataInner with thread-safe access.
    pub fn new() -> Self {
        tracing::trace!("creating new PagerData instance");
        PagerData {
            inner: Arc::new(Mutex::new(PagerDataInner::new())),
            mem_avail: Arc::new(Condvar::new()),
            mem_waiters: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Initialize the starting memory range for the pager.
    pub fn add_memory_range(&self, range: PhysRange) {
        // PRPTRACE (see nvme/dma.rs): catch garbage at the pool boundary.
        if range.start & 0xFFF != 0 || range.end & 0xFFF != 0 || range.start >= range.end {
            tracing::error!(
                "PRPTRACE bad donated range {:x}..{:x}",
                range.start,
                range.end
            );
        }
        let mut inner = self.inner.lock().unwrap();
        tracing::debug!("add memory range: {} pages", range.pages().count());
        if !inner.memory.try_add_memory_range(range) {
            if range.page_count() >= 128 {
                inner.memory.push(Region::new(range));
            } else {
                // The pool speaks byte addresses; `PhysRange::pages()` yields page *numbers*.
                // Freeing those numbers directly put values like 0x1a878 into the pool, which
                // went out as DMA targets: misaligned ones the device rejected (PRP Offset
                // Invalid), page-aligned ones it silently wrote disk data over low physical
                // memory. Only reachable for small donations that fail to merge -- i.e. the
                // fragmented-donation regime -- which is why it hid behind the donation wedge
                // (pagerwedge.md).
                let mut addr = range.start;
                while addr < range.end {
                    inner.memory.free_page(addr);
                    addr += PAGE;
                }
            }
        }
        drop(inner);
        self.mem_avail.notify_all();
    }

    fn do_fill_pages(
        &self,
        ctx: &'static PagerContext,
        id: ObjID,
        obj_range: ObjectRange,
        _partial: bool,
    ) -> Result<object_store::Vec<PagedPhysMem, INLINE_LEN>> {
        let start_page = obj_range.pages().next().unwrap();
        // For a single-page request the clamp cannot bind -- `1.min(max).max(1)` is 1 for every
        // value of max -- so skip avail_mem(), which folds over every region under the inner
        // lock, on the hottest path.
        let nr_pages = if obj_range.page_count() <= 1 {
            1
        } else {
            let current_mem_pages = ctx.data.avail_mem() / PAGE as usize;
            let max_pages = (current_mem_pages / 2).min(4096 * 128);
            tracing::trace!(
                "req: {}, cur: {} ({})",
                obj_range.pages().count(),
                current_mem_pages,
                current_mem_pages / 2
            );
            obj_range.page_count().min(max_pages).max(1)
        };
        let mut reqs = [PageRequest::new(start_page as i64, nr_pages as u32)];
        let count = page_in_many(ctx, id, &mut reqs)?;
        if count == 0 {
            // TODO: free pages in incomplete requests.
            todo!();
        }

        Ok(reqs.into_iter().next().unwrap().into_list())
    }
    /// Allocate a memory page and associate it with an object and range.
    /// Page in the data from disk
    /// Returns the physical range corresponding to the allocated page.
    pub fn fill_mem_pages_partial(
        &self,
        ctx: &'static PagerContext,
        id: ObjID,
        obj_range: ObjectRange,
    ) -> Result<object_store::Vec<PagedPhysMem, INLINE_LEN>> {
        // TODO: will need to check if the range contains this, not just starts here.
        if obj_range.start == (MAX_SIZE as u64) - PAGE {
            return Ok(self
                .fill_mem_pages_legacy(ctx, id, obj_range)?
                .into_iter()
                .map(|p| PagedPhysMem::new(p.1).completed())
                .collect());
        }

        let pages = self.do_fill_pages(ctx, id, obj_range, true)?;

        {
            let mut inner = self.inner.lock().unwrap();
            inner.recent_stats.read_pages(id, pages.len());
        }

        Ok(pages)
    }

    /// Allocate a memory page and associate it with an object and range.
    /// Page in the data from disk
    /// Returns the physical range corresponding to the allocated page.
    pub fn fill_mem_pages_legacy(
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
            r.push((range, self.fill_mem_page(ctx, id, range)?));
        }
        Ok(r)
    }
    /// Allocate a memory page and associate it with an object and range.
    /// Page in the data from disk
    /// Returns the physical range corresponding to the allocated page.
    pub fn fill_mem_page(
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

        let phys_range = page_in(ctx, id, obj_range)?;

        {
            let mut inner = self.inner.lock().unwrap();
            inner
                .recent_stats
                .read_pages(id, obj_range.len() / PAGE as usize);
        }

        return Ok(phys_range);
    }

    /// Answer the kernel's "does this object exist, and what is it" question -- and settle its
    /// metadata while we are here.
    ///
    /// The kernel reads the meta page on the first `check_id` of every object, which without this
    /// is a second round trip back to us, charged to whoever is mapping (mapperf.md: half of
    /// `insert_object`).
    ///
    /// The two backings need opposite things. An external file's metadata is invented, so we can
    /// state it and send no page at all. A stored object's metadata is on disk, and `page_in` gets
    /// it into a physical page over the disk's own path -- so we send the page but cannot vouch
    /// for its contents, having never looked at them. Either way the kernel stops faulting for it.
    pub fn lookup_object(&self, ctx: &'static PagerContext, id: ObjID) -> Result<ObjectInfo> {
        tracing::trace!(
            "lookup_object: {:?} (ino = {:?})",
            id,
            objid_to_ino(id.raw())
        );
        let base = ObjectInfo::new(
            LifetimeType::Persistent,
            BackingType::Normal,
            0.into(),
            0,
            Protections::empty(),
        );

        if objid_to_ino(id.raw()).is_some() {
            // No page crosses over. These objects have no stored metadata -- we invent it from
            // [EXTERNAL_META] plus the file's length -- so the length is the only thing the kernel
            // needs to build the page for itself. Filling one here instead would mean
            // `fill_physical_pages`, i.e. a `CopyUserPhys` on the strictly single-outstanding
            // pager->kernel channel (pagerperf.md 5), to hand over bytes we already know.
            let info = ObjectInfo::new(
                LifetimeType::Persistent,
                BackingType::Normal,
                EXTERNAL_META.kuid,
                EXTERNAL_META.nonce.0,
                EXTERNAL_META.default_prot,
            )
            .validated();
            let len_start = crate::dispatch_stats::DispatchStats::now_ns();
            // Deliberately not `?`: this path never checked existence, and returning an error here
            // would make the kernel cache the ID in `no_exist` permanently. Without a length we
            // just skip the synthesis and the meta page gets faulted in later, as it used to be.
            let len = ctx.paged_ostore(None)?.len_mtime_nlink(id.raw());
            // The segment now covers mtime as well, which used to sit outside it while costing a
            // whole fs-lock acquisition. `store-len` figures are not comparable across this change.
            crate::dispatch_stats::DISPATCH_STATS.info_lookup(
                crate::dispatch_stats::DispatchStats::now_ns() - len_start,
                None,
            );
            return Ok(match len {
                Ok((len, mtime, nlink)) => info.synth_meta(len).with_mtime(mtime).with_nlink(nlink),
                Err(e) => {
                    tracing::debug!("no length for external file {}: {}", id, e);
                    info
                }
            });
        }

        // This length was previously computed purely as an existence check and discarded. Stating
        // it lets the kernel answer faults past the end of the object without a round trip at all;
        // leaving it unstated is what made every stored object look zero-length to the kernel.
        let len_start = crate::dispatch_stats::DispatchStats::now_ns();
        let base = match ctx.paged_ostore(None)?.len(id.raw()) {
            Ok(len) => base.with_size(len),
            Err(_) => return Err(ObjectError::NoSuchObject.into()),
        };
        let meta_start = crate::dispatch_stats::DispatchStats::now_ns();

        // Best-effort: a failure here costs the kernel a page-in later, which is what it did
        // before, so it is not worth failing the lookup over.
        let meta = page_in(
            ctx,
            id,
            ObjectRange::new(MAX_SIZE as u64 - PAGE, MAX_SIZE as u64),
        );
        crate::dispatch_stats::DISPATCH_STATS.info_lookup(
            meta_start - len_start,
            Some(crate::dispatch_stats::DispatchStats::now_ns() - meta_start),
        );
        match meta {
            // Asserted unconditionally: the pager vouches for every object it serves, so the
            // kernel never hashes to check an id. Where a meta page comes with it, the kernel
            // takes `default_prot` from that page; where the prefetch failed there is nothing to
            // take it from and `ObjectInfo`'s zero stands.
            Ok(meta) => Ok(base.with_meta_page(meta).validated()),
            Err(e) => {
                tracing::debug!("failed to prefetch meta page for {}: {}", id, e);
                Ok(base.validated())
            }
        }
    }

    pub fn sync_region(
        &self,
        ctx: &'static PagerContext,
        info: &ObjectEvictInfo,
        work: &crate::watchdog::Work,
    ) -> CompletionToKernel {
        let po = {
            let mut inner = self.inner.lock().unwrap();
            inner.get_per_object(info.obj_id).clone()
        };

        let (count, compl) = po.sync_region(ctx, info, work);
        if count > 0 {
            let mut inner = self.inner.lock().unwrap();
            inner.recent_stats.write_pages(info.obj_id, count);
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
            .insert(comp, PagerClient::new(comp)?)
            .ok_or(ResourceError::OutOfResources.into())
    }

    pub fn drop_handle(&self, comp: ObjID, ds: Descriptor) -> Option<ObjectHandle> {
        let mut inner = self.inner.lock().unwrap();
        let pc = inner.handles.remove(comp, ds)?;
        Some(pc.into_handle())
    }
}
