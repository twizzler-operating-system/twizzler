use std::sync::Arc;

use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCommand, KernelCompletionData, ObjectInfo,
    ObjectRange, PhysRange, RequestFromKernel, RequestFromPager,
};
use twizzler_object::ObjID;
use twizzler_queue::QueueSender;

use crate::{data::PagerData, PagerContext};

async fn page_data_req(ctx: &PagerContext, id: ObjID, range: ObjectRange) -> Option<PhysRange> {
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
    ctx: &PagerContext,
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
        KernelCommand::ObjectSync(obj_id) => {
            ctx.data.sync(ctx, obj_id).await;
            Some(CompletionToKernel::new(KernelCompletionData::SyncOkay(
                obj_id,
            )))
        }
        KernelCommand::ObjectDel(obj_id) => {
            if ctx.ostore.unlink_object(obj_id.raw()).is_ok() {
                let _ = ctx
                    .ostore
                    .advance_epoch()
                    .inspect_err(|e| tracing::warn!("failed to advance epoch: {}", e));
            }
            Some(CompletionToKernel::new(KernelCompletionData::SyncOkay(
                obj_id,
            )))
        }
        KernelCommand::ObjectCreate(object_info) => {
            let _ = ctx.ostore.unlink_object(object_info.obj_id.raw());
            if let Err(e) = ctx.ostore.do_create_object(object_info.obj_id.raw()) {
                tracing::warn!("failed to create object {}: {}", object_info.obj_id, e);
                Some(CompletionToKernel::new(KernelCompletionData::Error))
            } else {
                // TODO: REMOVE ONCE WE HAVE RANDOM ACCESS
                let buf = [0; 0x1000 * 8];
                let _ = ctx
                    .ostore
                    .write_all(object_info.obj_id.raw(), &buf, 0)
                    .inspect_err(|e| {
                        tracing::warn!(
                            "failed to write pager info page for object {}: {}",
                            object_info.obj_id,
                            e
                        )
                    });
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
    }
}
