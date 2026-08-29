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

use crate::{helpers::PAGE, watchdog, PagerContext};

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

/// Largest "urgent" segment, in pages. The kernel only ever blocks on a page or two; this bounds
/// what an unusually large or malformed required range can claim as urgent.
const REQUIRED_SEGMENT_LIMIT: u64 = 16;

/// Bytes covered by one large page, i.e. the granularity the kernel merges 4 KiB frames at.
const LARGE_REGION: u64 = 2 * 1024 * 1024;

/// Whether a required range whose whole large-page region is being requested is served as that
/// region entire, instead of as a short urgent segment. Trading the early wake for the merge, but
/// only where a merge is actually possible; see `largepager.md`. `false` restores the unconditional
/// short segment.
const WHOLE_REGION_FOR_LARGE: bool = true;

/// The large-page region containing `offset`, if serving it whole could produce a large page.
///
/// Two ways it could not, and both fall back to the short urgent segment:
///
/// - **Region 0.** Object page 0 is the null page and is never delivered (`req_range.start` is
///   rewritten past it below), so the region can never be fully populated and can never merge.
///   Splitting it is free, which matters because first touch of any object -- and the whole of any
///   object smaller than a region -- lands here.
/// - **A region the request does not cover.** The kernel only widens to a whole region when that
///   region's level-1 entry is empty; a partially-populated region arrives as scattered pages that
///   no longer merge whatever the pager does, so rounding out would buy latency and nothing else.
fn whole_region(req_range: ObjectRange, offset: u64) -> Option<ObjectRange> {
    let start = offset - offset % LARGE_REGION;
    if !WHOLE_REGION_FOR_LARGE || start == 0 {
        return None;
    }
    let region = ObjectRange::new(start, start + LARGE_REGION);
    (req_range.start <= region.start && req_range.end >= region.end).then_some(region)
}

/// The piece of a page-data request a thread is actually blocked on: the first segment
/// [`transfer_segments`] emits, and the one whose completion wakes the faulter.
///
/// Equal to `req_range` when nothing in it is more urgent than the rest -- a prefetch, a caller
/// that needs all of it, or a required range being served as a whole large-page region for the
/// merge. Also what the fast-lane reservation sizes admission on ([`crate::threads`]), so that
/// decision and the transfer it predicts cannot drift apart.
pub(crate) fn urgent_segment(req_range: ObjectRange, required: ObjectRange) -> ObjectRange {
    let mut start = required.start.max(req_range.start);
    let mut end = required.end.min(req_range.end);
    if end > start {
        match whole_region(req_range, start) {
            Some(region) => {
                start = region.start;
                end = region.end;
            }
            None => end = end.min(start + REQUIRED_SEGMENT_LIMIT * PAGE),
        }
    }
    if end > start && (start > req_range.start || end < req_range.end) {
        ObjectRange::new(start, end)
    } else {
        req_range
    }
}

/// Order the pieces of a page-data request: the pages a thread is blocked on first, then the rest
/// in address order.
///
/// An empty `required`, or one that already spans the whole request, yields a single segment --
/// the old behaviour, and what a prefetch wants. The segments always cover exactly `req_range` and
/// never overlap, so no page is transferred twice.
fn transfer_segments(
    req_range: ObjectRange,
    required: ObjectRange,
) -> impl Iterator<Item = ObjectRange> {
    let urgent = urgent_segment(req_range, required);
    let mut segs: heapless::Vec<ObjectRange, 3> = heapless::Vec::new();
    let _ = segs.push(urgent);
    if urgent != req_range {
        if urgent.start > req_range.start {
            let _ = segs.push(ObjectRange::new(req_range.start, urgent.start));
        }
        if urgent.end < req_range.end {
            let _ = segs.push(ObjectRange::new(urgent.end, req_range.end));
        }
    }
    segs.into_iter()
}

/// Transfer one segment, emitting a completion per contiguous physical run.
///
/// `req_range` is passed only so diagnostics name the range the kernel asked about rather than the
/// piece being worked on.
fn transfer_range(
    ctx: &'static PagerContext,
    qid: u32,
    id: ObjID,
    seg: ObjectRange,
    req_range: ObjectRange,
    prefetch: bool,
) -> std::result::Result<(), ()> {
    let total = seg.page_count() as u64;
    tracing::trace!(
        "handling page data request for {}: {:?} of {:?} ({} pages) (prefetch = {})",
        id,
        seg,
        req_range,
        total,
        prefetch
    );
    let mut count = 0;
    while count < total {
        let range = ObjectRange::new(seg.start + count * PAGE, seg.end);
        let pages = match ctx
            .data
            .fill_mem_pages_partial(ctx, id, range)
            .inspect_err(|e| tracing::warn!("page data request failed: {}", e))
        {
            Ok(pages) => pages,
            Err(e) => {
                let comp = CompletionToKernel::new(
                    KernelCompletionData::Error(RawTwzError::new(e.raw())),
                    KernelCompletionFlags::DONE,
                );
                ctx.notify_kernel(qid, comp);
                return Err(());
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
                seg,
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
            let start = seg.start + (count + acc as u64) * PAGE;
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
    Ok(())
}

fn handle_page_data_request_task(
    ctx: &'static PagerContext,
    qid: u32,
    id: ObjID,
    mut req_range: ObjectRange,
    flags: PagerFlags,
    required: ObjectRange,
) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static PCOUNT: AtomicU64 = AtomicU64::new(0);
    let prefetch = flags.contains(PagerFlags::PREFETCH);

    if req_range.start == 0 {
        req_range.start = PAGE;
    }
    let start_time = Instant::now();
    let obj_len = ctx.paged_ostore(None).unwrap().len(id.raw()).ok();
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
    // A range lying *wholly* beyond the object has nothing to serve. The two clamps above cover a
    // range that straddles `max_len` and one that begins exactly at it, but not one that begins
    // past it -- which nothing produced while the kernel sent a widened region as a single
    // contiguous request, since such a request always straddled. Split per region, a short file's
    // second request starts beyond the end, matched neither case, and was served in full as holes:
    // pages delivered went 11k -> 24.9k on `pagepar` for the same 8018 the reader wanted.
    //
    // "Nothing to serve" is only a safe answer when nothing is *blocked* on it, which is why the
    // comparison to a declined prefetch does not carry: a prefetch may be declined precisely
    // because no thread is waiting for it. A demand fault is the opposite case, and acking it
    // empty wedges the guest outright.
    //
    // `ensure_in_core_pager`'s clamp keeps the faulting page in the range it asks for whatever
    // `known_len` says (that is what its `max(asked_end)` is for, since an object's metadata lives
    // past the data length), so "a later fault asks again for a range that does exist" is false --
    // the later fault is the *same* fault, asks the same range, and gets the same empty answer.
    // Nothing else fills the page: `ZERO_FILL_PAST_EOF` is off on the kernel side, deliberately,
    // which leaves serving a hole to this side. Observed as an unbounded refault loop on a write
    // into a sparse region -- 2.1M identical page-data requests, the store never read once, the
    // faulting thread never advancing.
    //
    // So decline only what nobody is waiting for, and serve the blocked pages as holes. Narrowing
    // to `required` rather than falling through keeps the read amplification this early return was
    // added to fix: the speculative tail past the end is still never transferred.
    if req_range.start > max_len {
        if required.start >= required.end {
            tracing::debug!(
                "page-data request for {} starts past the object ({:?} vs len {}); nothing to serve",
                id,
                req_range,
                max_len,
            );
            let done =
                CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::DONE);
            ctx.notify_kernel(qid, done);
            return;
        }
        let start = required.start.max(PAGE).min(MAX_SIZE as u64 - PAGE);
        let end = required.end.max(start + PAGE).min(MAX_SIZE as u64);
        tracing::debug!(
            "page-data request for {} starts past the object ({:?} vs len {}); serving blocked \
             range {:?} as holes",
            id,
            req_range,
            max_len,
            ObjectRange::new(start, end),
        );
        req_range = ObjectRange::new(start, end);
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

    // Serve what someone is blocked on before the rest.
    //
    // The kernel widens a one-page touch to a whole large-page region, so most of a page-data
    // request is speculative and nobody waits on it -- but it all used to arrive as a single
    // completion, so the faulting thread slept through the entire transfer to get its one page
    // (`pagerperf.md` 11). Transferring the required subrange as its own batch lets the kernel wake
    // it after tens of kilobytes instead.
    //
    // The segments are disjoint, so no page moves twice. The required range is by construction
    // inside the request's *first* large-page region -- `ensure_in_core_pager` aligns down to the
    // region containing the fault -- so splitting could only ever cost that region's merge, and
    // `whole_region` declines to split where the merge is still on the table.
    for range in transfer_segments(req_range, required) {
        if transfer_range(ctx, qid, id, range, req_range, prefetch).is_err() {
            return;
        }
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

fn handle_page_data_request(
    ctx: &'static PagerContext,
    qid: u32,
    id: ObjID,
    req_range: ObjectRange,
    flags: PagerFlags,
    required: ObjectRange,
    req: RequestFromKernel,
) -> Option<CompletionToKernel> {
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
        return Some(CompletionToKernel::new(
            KernelCompletionData::Okay,
            KernelCompletionFlags::DONE,
        ));
    }
    // Its own `Work`: the caller's ends when this returns, and the watchdog should be naming the
    // transfer rather than the dispatch that started it. The task sends its own DONE completion
    // through `ctx.notify_kernel`, which is why there is still nothing to return here.
    let _work = watchdog::begin("pagedata-task", qid, req);
    handle_page_data_request_task(ctx, qid, id, req_range, flags, required);
    None
}

fn object_info_req(ctx: &'static PagerContext, id: ObjID) -> Result<ObjectInfo> {
    ctx.data.lookup_object(ctx, id)
}

/// Detached, for the same reason page-data requests are.
///
/// `lookup_object` now fills the object's meta page (mapperf.md), which means this request does
/// real I/O and a physrw round trip. Run inline it holds a whole worker lane for its duration --
/// and that lane is one of the ones page-data requests need, during a read phase that is mostly
/// page-data. Handing the kernel its completion from a task instead frees the lane immediately.
fn handle_object_info_request(
    ctx: &'static PagerContext,
    qid: u32,
    obj_id: ObjID,
    req: RequestFromKernel,
) -> Option<CompletionToKernel> {
    // Its own `Work`, for the same reason page-data requests take one.
    let _work = watchdog::begin("info-task", qid, req);
    let start = crate::dispatch_stats::DispatchStats::now_ns();
    {
        let data = match object_info_req(ctx, obj_id) {
            Ok(info) => KernelCompletionData::ObjectInfoCompletion(obj_id, info),
            Err(e) => KernelCompletionData::Error(e.into()),
        };
        ctx.notify_kernel(
            qid,
            CompletionToKernel::new(data, KernelCompletionFlags::DONE),
        );
        crate::dispatch_stats::DISPATCH_STATS
            .info_task(crate::dispatch_stats::DispatchStats::now_ns() - start);
    }
    None
}

fn handle_sync_region(
    ctx: &'static PagerContext,
    id: u32,
    info: ObjectEvictInfo,
    req: RequestFromKernel,
    work: &watchdog::Work,
) -> Option<CompletionToKernel> {
    tracing::trace!("sync request: {:?}", info);
    if !info.flags.contains(ObjectEvictFlags::SYNC) {
        return Some(CompletionToKernel::new(
            KernelCompletionData::Error(TwzError::NOT_SUPPORTED.into()),
            KernelCompletionFlags::DONE,
        ));
    }

    if info.flags.contains(ObjectEvictFlags::FENCE) {
        // A fence sync is acked twice: an immediate non-DONE `Okay` to say the pager has taken it,
        // then the real DONE completion once the data is on disk. Detaching used to give that
        // ordering for free -- the ack was this function's return value and the task ran after it.
        // Doing the sync inline means sending the ack *here*, before starting, or the kernel would
        // see it after the completion it is supposed to precede.
        ctx.notify_kernel(
            id,
            CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::empty()),
        );
        let work = watchdog::begin("sync-task", id, req);
        let comp = ctx.data.sync_region(ctx, &info, &work);
        work.phase("notify-kernel");
        ctx.notify_kernel(id, comp);
        None
    } else {
        Some(ctx.data.sync_region(ctx, &info, work))
    }
}

/// Whether `ObjectCreate` unlinks the id before creating it.
///
/// It used to, unconditionally, to guarantee a create lands on a clean object. For a *fresh* id --
/// which is what the kernel sends almost every time -- that unlink is a global-fs-lock acquisition
/// and a full directory lookup whose only possible outcome is `NotFound`, i.e. one of the six store
/// round trips per create doing no work but paying for block reads. `create` now implies `O_TRUNC`
/// (see `Ext4Store::do_get_object_as_file`), so the clean-object guarantee comes from the lookup
/// the create was already doing.
///
/// Kept as a constant rather than deleted so the two can be A/B'd from one build: this sits on the
/// path of a 2x regression that is not yet explained, and being able to put the probe back without
/// a source change is worth one `if`.
const CREATE_PROBE_DELETE: bool = false;

/// Restore the redundant post-delete flush. See the note at its site.
const DEL_EXTRA_FLUSH: bool = false;

pub fn handle_kernel_request(
    ctx: &'static PagerContext,
    qid: u32,
    request: RequestFromKernel,
    work: &watchdog::Work,
) -> Option<CompletionToKernel> {
    let data = match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range, flags, required) => {
            work.phase("pagedata:spawn");
            return handle_page_data_request(ctx, qid, obj_id, range, flags, required, request);
        }
        KernelCommand::ObjectInfoReq(obj_id) => {
            work.phase("info:spawn");
            return handle_object_info_request(ctx, qid, obj_id, request);
        }
        KernelCommand::ObjectDel(obj_id) => match ctx.paged_ostore(None) {
            Ok(po) => {
                work.phase("del:delete");
                match po.delete_object(obj_id.raw()) {
                    Ok(_) => {
                        // `delete_object` flushes internally after `remove_file`, so this second
                        // flush re-took the global fs lock for an already-empty dirty list.
                        // Measured at 891us mean, 6.0% of `pager_create_delete_persistent`.
                        if DEL_EXTRA_FLUSH {
                            work.phase("del:flush");
                            let _ = po.flush();
                        }
                        KernelCompletionData::Okay
                    }
                    Err(e) => KernelCompletionData::Error(TwzError::from(e).into()),
                }
            }
            Err(e) => KernelCompletionData::Error(TwzError::from(e).into()),
        },
        KernelCommand::ObjectCreate(id, object_info) => match ctx.paged_ostore(None) {
            Ok(po) => {
                if CREATE_PROBE_DELETE {
                    work.phase("create:delete-existing");
                    let _ = po.delete_object(id.raw());
                }
                work.phase("create:create");
                match po.create_object(id.raw()) {
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
                            .unwrap();

                        work.phase("create:read-back");
                        ctx.paged_ostore(None)
                            .unwrap()
                            .read_object(id.raw(), 0, &mut buffer)
                            .unwrap();

                        // Validated, not merely echoed. The kernel derived `id` from these very
                        // kuid/nonce/def_prot fields and we have just written them into the meta
                        // page, so `check_id` would read back exactly what it already knows.
                        // Without this the first map of every pager-backed object pays a meta-page
                        // page-in for an answer nobody had to look up (mapperf.md).
                        KernelCompletionData::ObjectInfoCompletion(id, object_info.validated())
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
            return handle_sync_region(ctx, qid, info, request, work);
        }
    };

    tracing::debug!("done; sending response: {:?}", data);
    Some(CompletionToKernel::new(data, KernelCompletionFlags::DONE))
}
