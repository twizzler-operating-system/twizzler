use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use object_store::PagedObjectStore;
use twizzler::{
    error::RawTwzError,
    object::{MetaFlags, MetaInfo, ObjID},
};
use twizzler_abi::{
    object::MAX_SIZE,
    pager::{
        CompletionToKernel, KernelCommand, KernelCompletionData, KernelCompletionFlags,
        ObjectEvictFlags, ObjectEvictInfo, ObjectInfo, ObjectRange, PageFlags, PagerFlags,
        PhysRange, RequestFromKernel,
    },
};
use twizzler_rt_abi::{error::TwzError, object::Nonce, Result};

use crate::{helpers::PAGE, threads::spawn_async, watchdog, PagerContext};

/// In-flight page-data requests, with high-water marks.
///
/// Answers the question downstream instrumentation cannot: whether the pager ever services more
/// than one paging request at a time. If it does not, no amount of per-object locking further down
/// can produce parallelism -- there is nothing concurrent to serialize in the first place.
struct ReqStats {
    demand: AtomicU64,
    prefetch: AtomicU64,
    max_demand: AtomicU64,
    max_prefetch: AtomicU64,
    max_total: AtomicU64,
    completed: AtomicU64,
}

static REQ_STATS: ReqStats = ReqStats {
    demand: AtomicU64::new(0),
    prefetch: AtomicU64::new(0),
    max_demand: AtomicU64::new(0),
    max_prefetch: AtomicU64::new(0),
    max_total: AtomicU64::new(0),
    completed: AtomicU64::new(0),
};

/// Most prefetch page-data requests to work on at once. Small on purpose: `REQSTATS` has never
/// shown more than 3-4 concurrent *demand* requests, so speculation is allowed a fraction of that
/// rather than a share proportional to the worker pool.
const MAX_INFLIGHT_PREFETCH: u64 = 2;

/// Prefetches turned away by the cap. Worth counting separately from completions: a high number
/// means the kernel is speculating far ahead of what the pager can absorb, which is a reason to
/// prefetch less rather than to raise the cap.
static DECLINED_PREFETCH: AtomicU64 = AtomicU64::new(0);

/// Decrements its gauge on drop, so the error returns out of the fill loop cannot leak a count. A
/// leaked gauge would only ever *overstate* concurrency, which is the one direction a measurement
/// of concurrency must not drift.
struct ReqGuard {
    prefetch: bool,
}

impl ReqStats {
    fn enter(&'static self, prefetch: bool) -> ReqGuard {
        let (gauge, max) = if prefetch {
            (&self.prefetch, &self.max_prefetch)
        } else {
            (&self.demand, &self.max_demand)
        };
        let now = gauge.fetch_add(1, Ordering::AcqRel) + 1;
        max.fetch_max(now, Ordering::AcqRel);
        let total = self.demand.load(Ordering::Acquire) + self.prefetch.load(Ordering::Acquire);
        self.max_total.fetch_max(total, Ordering::AcqRel);
        ReqGuard { prefetch }
    }
}

impl Drop for ReqGuard {
    fn drop(&mut self) {
        let stats = &REQ_STATS;
        if self.prefetch {
            stats.prefetch.fetch_sub(1, Ordering::AcqRel);
        } else {
            stats.demand.fetch_sub(1, Ordering::AcqRel);
        }
        let done = stats.completed.fetch_add(1, Ordering::Relaxed) + 1;
        if done.is_power_of_two() {
            tracing::info!(
                "REQSTATS: {} page-data requests done ({} prefetch declined); in flight now {} demand / {} prefetch; max {} demand, {} prefetch, {} total",
                done,
                DECLINED_PREFETCH.load(Ordering::Relaxed),
                stats.demand.load(Ordering::Relaxed),
                stats.prefetch.load(Ordering::Relaxed),
                stats.max_demand.load(Ordering::Relaxed),
                stats.max_prefetch.load(Ordering::Relaxed),
                stats.max_total.load(Ordering::Relaxed),
            );
        }
    }
}

async fn handle_page_data_request_task(
    ctx: &'static PagerContext,
    qid: u32,
    id: ObjID,
    mut req_range: ObjectRange,
    flags: PagerFlags,
) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static PCOUNT: AtomicU64 = AtomicU64::new(0);
    let prefetch = flags.contains(PagerFlags::PREFETCH);

    if req_range.start == 0 {
        req_range.start = PAGE;
    }
    let start_time = Instant::now();
    let obj_len = ctx.paged_ostore(None).unwrap().len(id.raw()).await.ok();
    let max_len = obj_len
        .map(|x| x + PAGE)
        .unwrap_or(MAX_SIZE as u64)
        .min(MAX_SIZE as u64);
    if req_range.start >= MAX_SIZE as u64 {
        let done = CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::DONE);
        ctx.notify_kernel(qid, done);
        return;
    }
    if req_range.end > MAX_SIZE as u64 {
        req_range.end = MAX_SIZE as u64;
    }
    // TODO: need better logic to decide when we want to actually extend object size.
    if req_range.start < max_len && req_range.end > max_len {
        req_range.end = max_len.next_multiple_of(PAGE);
    }
    if req_range.start == max_len && req_range.end > max_len {
        req_range.end = max_len.next_multiple_of(PAGE) + PAGE;
    }
    if prefetch {
        tracing::info!("STARTING {}: {:?} {:?}", id, req_range, flags);
        if let Some(len) = obj_len {
            // Clamp only, never extend -- which is what "reduce len" always claimed. Assigning
            // here turned a kernel-side prefetch of one region into a whole-object read: a request
            // for ObjRange[1000-2000) came back as [1000-1c1e000), 29 MB, three concurrent, and
            // the guest wedged. The kernel decides how far to speculate; this only stops it
            // running off the end of the object.
            let end = (len.next_multiple_of(PAGE) + PAGE).min(req_range.end);
            tracing::debug!(
                "==> prefetch request reduce len: {} -> {}",
                req_range.end,
                end
            );
            req_range.end = end;
        }
        PCOUNT.fetch_add(1, Ordering::SeqCst);
    } else {
        COUNT.fetch_add(1, Ordering::SeqCst);
    }
    let _req = REQ_STATS.enter(prefetch);

    let total = req_range.page_count() as u64;
    tracing::trace!(
        "handling page data request for {}: {:?} ({} pages) (prefetch = {})",
        id,
        req_range,
        total,
        prefetch
    );
    let mut count = 0;
    while count < total {
        tracing::trace!(
            "reading {} page {} of {} (pre = {})",
            id,
            count,
            total,
            prefetch
        );
        let range = ObjectRange::new(req_range.start + count * PAGE, req_range.end);
        let pages = match ctx
            .data
            .fill_mem_pages_partial(ctx, id, range)
            .await
            .inspect_err(|e| tracing::warn!("page data request failed: {}", e))
        {
            Ok(pages) => pages,
            Err(e) => {
                let comp = CompletionToKernel::new(
                    KernelCompletionData::Error(RawTwzError::new(e.raw())),
                    KernelCompletionFlags::DONE,
                );
                ctx.notify_kernel(qid, comp);
                return;
            }
        };

        let thiscount = pages
            .iter()
            .fold(0u64, |acc, x| acc + (x.range.end - x.range.start) / PAGE);

        // A fill that yields no pages (e.g. out of physical memory) would spin this loop
        // forever. Report what we did manage and let the kernel re-fault for the rest.
        if thiscount == 0 {
            tracing::warn!(
                "page data request for {} {:?} made no progress after {} of {} pages",
                id,
                req_range,
                count,
                total
            );
            break;
        }

        // try to compress page ranges
        let runs = crate::helpers::consecutive_slices(pages.as_slice(), |a, b| {
            a.range.end == b.range.start && a.same_flags(b)
        });
        let mut acc = 0;
        for comp in runs.map(|run| {
            let start = run[0];
            let last = run.last().unwrap();
            let flags = if start.is_wired() {
                PageFlags::WIRED
            } else {
                PageFlags::empty()
            };
            let phys_range = PhysRange {
                start: start.range.start,
                end: last.range.end,
            };
            tracing::trace!("{:?} ==> {:?} {:?}", id, req_range, phys_range);

            let start = req_range.start + (count + acc as u64) * PAGE;
            let range = ObjectRange::new(start, start + phys_range.len() as u64);

            acc += phys_range.page_count();
            CompletionToKernel::new(
                KernelCompletionData::PageDataCompletion(id, range, phys_range, flags),
                KernelCompletionFlags::empty(),
            )
        }) {
            ctx.notify_kernel(qid, comp);
        }
        count += thiscount;
    }
    if prefetch {
        PCOUNT.fetch_sub(1, Ordering::SeqCst);
    } else {
        COUNT.fetch_sub(1, Ordering::SeqCst);
    }
    if prefetch {
        tracing::info!(
            "COMPLETED: {} {:?} in {} ms, {}:{} remaining",
            id,
            req_range,
            start_time.elapsed().as_millis(),
            COUNT.load(Ordering::SeqCst),
            PCOUNT.load(Ordering::SeqCst),
        );
    }

    let done = CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::DONE);
    ctx.notify_kernel(qid, done);
}

async fn handle_page_data_request(
    ctx: &'static PagerContext,
    qid: u32,
    id: ObjID,
    req_range: ObjectRange,
    flags: PagerFlags,
    req: RequestFromKernel,
) -> Vec<CompletionToKernel> {
    tracing::debug!(
        "{}: {:?} {} pages",
        id,
        req_range,
        req_range.pages().count()
    );
    // Speculation must never crowd out a demand fault. A prefetch occupies a worker for its whole
    // transfer, so an unbounded burst of them can take every bulk lane and put real faults behind
    // work nobody has asked for yet (pagerplan.md, stage 3). Over the cap we simply decline: the
    // kernel never waits on a prefetch, so acking it is a complete answer, and the pages get read
    // on demand if they are ever actually wanted.
    if flags.contains(PagerFlags::PREFETCH)
        && REQ_STATS.prefetch.load(Ordering::Acquire) >= MAX_INFLIGHT_PREFETCH
    {
        DECLINED_PREFETCH.fetch_add(1, Ordering::Relaxed);
        return vec![CompletionToKernel::new(
            KernelCompletionData::Okay,
            KernelCompletionFlags::DONE,
        )];
    }
    // Detached: the caller's `Work` ends when this returns, so the task needs its own.
    spawn_async(async move {
        let _work = watchdog::begin("pagedata-task", qid, req);
        handle_page_data_request_task(ctx, qid, id, req_range, flags).await;
    });
    vec![]
}

async fn object_info_req(ctx: &'static PagerContext, id: ObjID) -> Result<ObjectInfo> {
    ctx.data.lookup_object(ctx, id).await
}

/// Detached, for the same reason page-data requests are.
///
/// `lookup_object` now fills the object's meta page (mapperf.md), which means this request does
/// real I/O and a physrw round trip. Run inline it holds a whole worker lane for its duration --
/// and that lane is one of the ones page-data requests need, during a read phase that is mostly
/// page-data. Handing the kernel its completion from a task instead frees the lane immediately.
async fn handle_object_info_request(
    ctx: &'static PagerContext,
    qid: u32,
    obj_id: ObjID,
    req: RequestFromKernel,
) -> Vec<CompletionToKernel> {
    // Detached: the caller's `Work` ends when this returns, so the task needs its own.
    spawn_async(async move {
        let _work = watchdog::begin("info-task", qid, req);
        let data = match object_info_req(ctx, obj_id).await {
            Ok(info) => KernelCompletionData::ObjectInfoCompletion(obj_id, info),
            Err(e) => KernelCompletionData::Error(e.into()),
        };
        ctx.notify_kernel(
            qid,
            CompletionToKernel::new(data, KernelCompletionFlags::DONE),
        );
    });
    vec![]
}

async fn handle_sync_region(
    ctx: &'static PagerContext,
    id: u32,
    info: ObjectEvictInfo,
    req: RequestFromKernel,
    work: &watchdog::Work,
) -> CompletionToKernel {
    tracing::trace!("sync request: {:?}", info);
    if !info.flags.contains(ObjectEvictFlags::SYNC) {
        return CompletionToKernel::new(
            KernelCompletionData::Error(TwzError::NOT_SUPPORTED.into()),
            KernelCompletionFlags::DONE,
        );
    }

    if info.flags.contains(ObjectEvictFlags::FENCE) {
        // Detached: the caller's `Work` ends when this returns, so the task needs its own.
        spawn_async(async move {
            let work = watchdog::begin("sync-task", id, req);
            let comp = ctx.data.sync_region(ctx, &info, &work).await;
            work.phase("notify-kernel");
            ctx.notify_kernel(id, comp);
        });
        CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::empty())
    } else {
        ctx.data.sync_region(ctx, &info, work).await
    }
}

pub async fn handle_kernel_request(
    ctx: &'static PagerContext,
    qid: u32,
    request: RequestFromKernel,
    work: &watchdog::Work,
) -> Vec<CompletionToKernel> {
    let data = match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range, flags) => {
            work.phase("pagedata:spawn");
            return handle_page_data_request(ctx, qid, obj_id, range, flags, request).await;
        }
        KernelCommand::ObjectInfoReq(obj_id) => {
            work.phase("info:spawn");
            return handle_object_info_request(ctx, qid, obj_id, request).await;
        }
        KernelCommand::ObjectDel(obj_id) => match ctx.paged_ostore(None) {
            Ok(po) => {
                work.phase("del:delete");
                match po.delete_object(obj_id.raw()).await {
                    Ok(_) => {
                        work.phase("del:flush");
                        let _ = po.flush().await;
                        KernelCompletionData::Okay
                    }
                    Err(e) => KernelCompletionData::Error(TwzError::from(e).into()),
                }
            }
            Err(e) => KernelCompletionData::Error(TwzError::from(e).into()),
        },
        KernelCommand::ObjectCreate(id, object_info) => match ctx.paged_ostore(None) {
            Ok(po) => {
                work.phase("create:delete-existing");
                let _ = po.delete_object(id.raw()).await;
                work.phase("create:create");
                match po.create_object(id.raw()).await {
                    Ok(_) => {
                        let mut buffer = [0; 0x1000];
                        let meta = MetaInfo {
                            nonce: Nonce(object_info.nonce),
                            kuid: object_info.kuid,
                            default_prot: object_info.def_prot,
                            flags: MetaFlags::empty(),
                            fotcount: 0,
                            extcount: 0,
                        };
                        unsafe fn any_as_u8_slice<T: Sized>(p: &T) -> &[u8] {
                            ::core::slice::from_raw_parts(
                                (p as *const T) as *const u8,
                                ::core::mem::size_of::<T>(),
                            )
                        }
                        unsafe {
                            buffer[0..size_of::<MetaInfo>()]
                                .copy_from_slice(any_as_u8_slice(&meta));
                        }
                        work.phase("create:write-meta");
                        ctx.paged_ostore(None)
                            .unwrap()
                            .write_object(id.raw(), 0, &buffer)
                            .await
                            .unwrap();

                        work.phase("create:read-back");
                        ctx.paged_ostore(None)
                            .unwrap()
                            .read_object(id.raw(), 0, &mut buffer)
                            .await
                            .unwrap();

                        KernelCompletionData::ObjectInfoCompletion(id, object_info)
                    }
                    Err(e) => {
                        tracing::warn!("failed to create object {}: {}", id, e);
                        KernelCompletionData::Error(TwzError::from(e).into())
                    }
                }
            }
            Err(e) => {
                tracing::warn!("failed to create object {}: {}", id, e);
                KernelCompletionData::Error(TwzError::from(e).into())
            }
        },
        KernelCommand::DramPages(phys_range) => {
            tracing::debug!("tracking {} KB memory", phys_range.len() / 1024);
            ctx.data.add_memory_range(phys_range);
            KernelCompletionData::Okay
        }
        KernelCommand::ObjectEvict(info) => {
            work.phase("evict");
            return vec![handle_sync_region(ctx, qid, info, request, work).await];
        }
    };

    tracing::debug!("done; sending response: {:?}", data);
    vec![CompletionToKernel::new(data, KernelCompletionFlags::DONE)]
}
