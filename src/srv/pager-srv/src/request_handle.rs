use blocking::unblock;
use twizzler::{
    error::RawTwzError,
    object::{MetaFlags, MetaInfo, ObjID},
};
use twizzler_abi::pager::{
    CompletionToKernel, KernelCommand, KernelCompletionData, KernelCompletionFlags,
    ObjectEvictFlags, ObjectEvictInfo, ObjectInfo, ObjectRange, RequestFromKernel,
};
use twizzler_rt_abi::{error::TwzError, object::Nonce, Result};

use crate::{helpers::PAGE, PagerContext, EXECUTOR};

async fn handle_page_data_request_task(
    ctx: &'static PagerContext,
    qid: u32,
    id: ObjID,
    req_range: ObjectRange,
) {
    let mut total = req_range.pages().count() as u64;
    let mut count = 0;
    while count < total {
        tracing::debug!("reading {} page {} of {}", id, count, total);
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
        let mut comps = pages
            .into_iter()
            .enumerate()
            .map(|(i, x)| {
                let start = req_range.start + (count + i as u64) * PAGE;
                let range = ObjectRange::new(start, start + PAGE);
                CompletionToKernel::new(
                    KernelCompletionData::PageDataCompletion(id, range, x),
                    KernelCompletionFlags::empty(),
                )
            })
            .collect::<Vec<_>>();
        for comp in comps {
            ctx.notify_kernel(qid, comp).await;
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
            handle_page_data_request_task(ctx, qid, id, req_range).await;
        })
        .detach();
    vec![]
    /*

    match ctx
        .data
        .fill_mem_pages(ctx, id, req_range)
        .await
        .inspect_err(|e| tracing::warn!("page data request failed: {}", e))
    {
        Ok(v) => {
            let mut r = v
                .into_iter()
                .map(|x| {
                    CompletionToKernel::new(
                        KernelCompletionData::PageDataCompletion(id, x.0, x.1),
                        KernelCompletionFlags::empty(),
                    )
                })
                .collect::<Vec<_>>();

            if let Some(last) = r.last_mut() {
                last.set_flags(KernelCompletionFlags::DONE);
            }
            //tracing::info!("page in done; sending response: {:?}", r);
            r
        }
        Err(e) => vec![CompletionToKernel::new(
            KernelCompletionData::Error(e.into()),
            KernelCompletionFlags::DONE,
        )],
    }
    */
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
        KernelCommand::PageDataReq(obj_id, range) => {
            return handle_page_data_request(ctx, qid, obj_id, range).await;
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
