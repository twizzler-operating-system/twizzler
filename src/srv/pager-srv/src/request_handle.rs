use blocking::unblock;
use twizzler::{
    error::RawTwzError,
    object::{MetaFlags, MetaInfo, ObjID},
};
use twizzler_abi::pager::{
    CompletionToKernel, KernelCommand, KernelCompletionData, KernelCompletionFlags,
    ObjectEvictFlags, ObjectEvictInfo, ObjectInfo, ObjectRange, PagerFlags, PhysRange,
    RequestFromKernel,
};
use twizzler_rt_abi::{error::TwzError, object::Nonce, Result};

use crate::{helpers::PAGE, PagerContext, EXECUTOR};

async fn handle_page_data_request_task(
    ctx: &'static PagerContext,
    qid: u32,
    id: ObjID,
    mut req_range: ObjectRange,
    flags: PagerFlags,
) {
    let prefetch = flags.contains(PagerFlags::PREFETCH);

    if prefetch {
        if let Ok(len) = blocking::unblock(move || ctx.paged_ostore.len(id.raw())).await {
            tracing::debug!(
                "==> prefetch request reduce len: {} -> {}",
                req_range.end,
                len
            );
            req_range.end = len.next_multiple_of(PAGE);
        }
    }

    let total = req_range.pages().count() as u64;
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
                ctx.notify_kernel(qid, comp).await;
                return;
            }
        };
        let thiscount = pages.len() as u64;
        // try to compress page ranges
        let runs = crate::helpers::consecutive_slices(pages.as_slice());
        let mut acc = 0;
        let pages = runs
            .map(|run| {
                let start = run[0];
                let last = run.last().unwrap();
                let ret = (acc, PhysRange::new(start.start, last.end));
                acc += run.len();
                ret
            })
            .collect::<Vec<_>>();
        let comps = pages
            .into_iter()
            .map(|(i, x)| {
                let start = req_range.start + (count + i as u64) * PAGE;
                let range = ObjectRange::new(start, start + x.len() as u64);
                CompletionToKernel::new(
                    KernelCompletionData::PageDataCompletion(id, range, x),
                    KernelCompletionFlags::empty(),
                )
            })
            .collect::<Vec<_>>();
        tracing::trace!("sending {} kernel notifs for {}", comps.len(), id);
        for comp in comps.iter() {
            ctx.notify_kernel(qid, *comp).await;
        }
        count += thiscount;
    }
    let done = CompletionToKernel::new(KernelCompletionData::Okay, KernelCompletionFlags::DONE);
    ctx.notify_kernel(qid, done).await;
}

async fn handle_page_data_request(
    ctx: &'static PagerContext,
    qid: u32,
    id: ObjID,
    req_range: ObjectRange,
    flags: PagerFlags,
) -> Vec<CompletionToKernel> {
    tracing::debug!(
        "{}: {:?} {} pages",
        id,
        req_range,
        req_range.pages().count()
    );
    let _task = EXECUTOR
        .get()
        .unwrap()
        .spawn(async move {
            handle_page_data_request_task(ctx, qid, id, req_range, flags).await;
        })
        .detach();
    vec![]
}

async fn object_info_req(ctx: &'static PagerContext, id: ObjID) -> Result<ObjectInfo> {
    ctx.data.lookup_object(ctx, id).await
}

async fn handle_sync_region(
    ctx: &'static PagerContext,
    info: ObjectEvictInfo,
) -> CompletionToKernel {
    if !info.flags.contains(ObjectEvictFlags::SYNC) {
        return CompletionToKernel::new(
            KernelCompletionData::Error(TwzError::NOT_SUPPORTED.into()),
            KernelCompletionFlags::DONE,
        );
    }
    ctx.data.sync_region(ctx, &info).await
}

pub async fn handle_kernel_request(
    ctx: &'static PagerContext,
    qid: u32,
    request: RequestFromKernel,
) -> Vec<CompletionToKernel> {
    tracing::trace!("handling kernel request {:?}", request);

    let data = match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range, flags) => {
            return handle_page_data_request(ctx, qid, obj_id, range, flags).await;
        }
        KernelCommand::ObjectInfoReq(obj_id) => match object_info_req(ctx, obj_id).await {
            Ok(info) => KernelCompletionData::ObjectInfoCompletion(obj_id, info),
            Err(e) => KernelCompletionData::Error(e.into()),
        },

        KernelCommand::ObjectDel(obj_id) => {
            unblock(move || {
                let res = ctx.paged_ostore.delete_object(obj_id.raw());
                match res {
                    Ok(_) => {
                        let _ = ctx
                            .paged_ostore
                            .flush()
                            .inspect_err(|e| tracing::warn!("failed to advance epoch: {}", e));
                        KernelCompletionData::Okay
                    }
                    Err(e) => KernelCompletionData::Error(TwzError::from(e).into()),
                }
            })
            .await
        }
        KernelCommand::ObjectCreate(id, object_info) => {
            blocking::unblock(move || {
                let _ = ctx.paged_ostore.delete_object(id.raw());
                match ctx.paged_ostore.create_object(id.raw()) {
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
                        ctx.paged_ostore.write_object(id.raw(), 0, &buffer).unwrap();

                        KernelCompletionData::ObjectInfoCompletion(id, object_info)
                    }
                    Err(e) => {
                        tracing::warn!("failed to create object {}: {}", id, e);
                        KernelCompletionData::Error(TwzError::from(e).into())
                    }
                }
            })
            .await
        }
        KernelCommand::DramPages(phys_range) => {
            tracing::debug!("tracking {} KB memory", phys_range.len() / 1024);
            ctx.data.add_memory_range(phys_range);
            KernelCompletionData::Okay
        }
        KernelCommand::ObjectEvict(info) => {
            return vec![handle_sync_region(ctx, info).await];
        }
    };

    tracing::debug!("done; sending response: {:?}", data);
    vec![CompletionToKernel::new(data, KernelCompletionFlags::DONE)]
}
