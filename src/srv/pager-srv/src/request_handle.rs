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
    let max_len = ctx
        .paged_ostore(None)
        .unwrap()
        .len(id.raw())
        .await
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
        if let Ok(len) = ctx.paged_ostore(None).unwrap().len(id.raw()).await {
            tracing::debug!(
                "==> prefetch request reduce len: {} -> {}",
                req_range.end,
                len
            );
            req_range.end = len.next_multiple_of(PAGE) + PAGE;
        }
        PCOUNT.fetch_add(1, Ordering::SeqCst);
    } else {
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

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

        // try to compress page ranges
        let runs = crate::helpers::consecutive_slices(pages.as_slice());
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
            work.phase("info:lookup");
            match object_info_req(ctx, obj_id).await {
                Ok(info) => KernelCompletionData::ObjectInfoCompletion(obj_id, info),
                Err(e) => KernelCompletionData::Error(e.into()),
            }
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
