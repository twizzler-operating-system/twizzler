use twizzler::object::ObjID;
use twizzler_abi::pager::{
    CompletionToKernel, KernelCommand, KernelCompletionData, KernelCompletionFlags, ObjectInfo,
    ObjectRange, PhysRange, RequestFromKernel,
};
use twizzler_rt_abi::{error::TwzError, Result};

use crate::PagerContext;

async fn handle_page_data_request(
    ctx: &'static PagerContext,
    id: ObjID,
    range: ObjectRange,
) -> Result<PhysRange> {
    ctx.data
        .fill_mem_page(ctx, id, range)
        .await
        .inspect_err(|e| tracing::warn!("page data request failed: {}", e))
}

fn object_info_req(ctx: &PagerContext, id: ObjID) -> Result<ObjectInfo> {
    ctx.data.lookup_object(ctx, id)
}

pub async fn handle_kernel_request(
    ctx: &'static PagerContext,
    request: RequestFromKernel,
) -> CompletionToKernel {
    tracing::debug!("handling kernel request {:?}", request);

    let data = match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range) => {
            match handle_page_data_request(ctx, obj_id, range).await {
                Ok(phys_range) => {
                    KernelCompletionData::PageDataCompletion(obj_id, range, phys_range)
                }
                Err(e) => KernelCompletionData::Error(e.into()),
            }
        }
        KernelCommand::ObjectInfoReq(obj_id) => match object_info_req(ctx, obj_id) {
            Ok(info) => KernelCompletionData::ObjectInfoCompletion(obj_id, info),
            Err(e) => KernelCompletionData::Error(e.into()),
        },

        KernelCommand::ObjectDel(obj_id) => match ctx.paged_ostore.delete_object(obj_id.raw()) {
            Ok(_) => {
                let _ = ctx
                    .paged_ostore
                    .flush()
                    .inspect_err(|e| tracing::warn!("failed to advance epoch: {}", e));
                KernelCompletionData::Okay
            }
            Err(e) => KernelCompletionData::Error(TwzError::from(e).into()),
        },
        KernelCommand::ObjectCreate(id, object_info) => {
            let _ = ctx.paged_ostore.delete_object(id.raw());
            match ctx.paged_ostore.create_object(id.raw()) {
                Ok(_) => KernelCompletionData::ObjectInfoCompletion(id, object_info),
                Err(e) => {
                    tracing::warn!("failed to create object {}: {}", id, e);
                    KernelCompletionData::Error(TwzError::from(e).into())
                }
            }
        }
        KernelCommand::DramPages(phys_range) => {
            tracing::debug!("tracking {} KB memory", phys_range.len() / 1024);
            ctx.data.add_memory_range(phys_range);
            KernelCompletionData::Okay
        }
        KernelCommand::ObjectEvict(info) => {
            ctx.data.sync(ctx, info.obj_id).await;
            KernelCompletionData::Okay
        }
    };

    CompletionToKernel::new(data, KernelCompletionFlags::DONE)
}
