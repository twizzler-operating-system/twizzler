use twizzler::object::ObjID;
use twizzler_abi::pager::{
    CompletionToKernel, KernelCommand, KernelCompletionData, ObjectInfo, ObjectRange, PhysRange,
    RequestFromKernel,
};

use crate::PagerContext;

async fn page_data_req(
    ctx: &'static PagerContext,
    id: ObjID,
    range: ObjectRange,
) -> Option<PhysRange> {
    ctx.data
        .fill_mem_page(ctx, id, range)
        .await
        .inspect_err(|e| tracing::warn!("page data request failed: {}", e))
        .ok()
}

fn object_info_req(ctx: &PagerContext, id: ObjID) -> Option<ObjectInfo> {
    ctx.data.lookup_object(ctx, id)
}

pub async fn handle_kernel_request(
    ctx: &'static PagerContext,
    request: RequestFromKernel,
) -> Option<CompletionToKernel> {
    tracing::debug!("handling kernel request {:?}", request);

    match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range) => Some(CompletionToKernel::new(
            if let Some(phys_range) = page_data_req(ctx, obj_id, range).await {
                KernelCompletionData::PageDataCompletion(obj_id, range, phys_range)
            } else {
                KernelCompletionData::Error
            },
        )),
        KernelCommand::ObjectInfoReq(obj_id) => {
            if let Some(obj_info) = object_info_req(ctx, obj_id) {
                Some(CompletionToKernel::new(
                    KernelCompletionData::ObjectInfoCompletion(obj_info),
                ))
            } else {
                Some(CompletionToKernel::new(KernelCompletionData::NoSuchObject(
                    obj_id,
                )))
            }
        }

        KernelCommand::ObjectDel(obj_id) => {
            if ctx.paged_ostore.delete_object(obj_id.raw()).is_ok() {
                let _ = ctx
                    .paged_ostore
                    .flush()
                    .inspect_err(|e| tracing::warn!("failed to advance epoch: {}", e));
            }
            Some(CompletionToKernel::new(KernelCompletionData::SyncOkay(
                obj_id,
            )))
        }
        KernelCommand::ObjectCreate(id, object_info) => {
            let _ = ctx.paged_ostore.delete_object(id.raw());
            if let Err(e) = ctx.paged_ostore.create_object(id.raw()) {
                tracing::warn!("failed to create object {}: {}", id, e);
                Some(CompletionToKernel::new(KernelCompletionData::Error))
            } else {
                Some(CompletionToKernel::new(
                    KernelCompletionData::ObjectInfoCompletion(object_info),
                ))
            }
        }
        KernelCommand::DramPages(phys_range) => {
            tracing::debug!("tracking {} MB memory", phys_range.len() / (1024 * 1024));
            ctx.data.init_range(phys_range);
            Some(CompletionToKernel::new(KernelCompletionData::Okay))
        }
        KernelCommand::ObjectEvict(obj_id) => {
            ctx.data.sync(ctx, obj_id).await;
            Some(CompletionToKernel::new(KernelCompletionData::Okay))
        }
    }
}
