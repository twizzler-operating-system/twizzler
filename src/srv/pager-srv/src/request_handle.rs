use std::sync::Arc;

use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCommand, KernelCompletionData, ObjectInfo,
    ObjectRange, PhysRange, RequestFromKernel, RequestFromPager,
};
use twizzler_object::ObjID;
use twizzler_queue::QueueSender;

use crate::data::PagerData;

async fn page_data_req(
    rq: &Arc<QueueSender<RequestFromPager, CompletionToPager>>,
    data: Arc<PagerData>,
    id: ObjID,
    range: ObjectRange,
) -> Option<PhysRange> {
    data.fill_mem_page(rq, id, range)
        .await
        .inspect_err(|e| tracing::warn!("page data request failed: {}", e))
        .ok()
}

fn object_info_req(data: Arc<PagerData>, id: ObjID) -> Option<ObjectInfo> {
    data.lookup_object(id)
}

pub async fn handle_kernel_request(
    rq: Arc<QueueSender<RequestFromPager, CompletionToPager>>,
    request: RequestFromKernel,
    data: Arc<PagerData>,
) -> Option<CompletionToKernel> {
    tracing::debug!("handling kernel request {:?}", request);

    match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range) => {
            tracing::debug!(
                "handling PageDataReq for ObjID: {:?}, Range: start = {}, end = {}",
                obj_id,
                range.start,
                range.end
            );
            Some(CompletionToKernel::new(
                if let Some(phys_range) = page_data_req(&rq, data, obj_id, range).await {
                    KernelCompletionData::PageDataCompletion(obj_id, range, phys_range)
                } else {
                    KernelCompletionData::Error
                },
            ))
        }
        KernelCommand::ObjectInfoReq(obj_id) => {
            tracing::trace!("handling ObjectInfo for ObjID: {:?}", obj_id);
            if let Some(obj_info) = object_info_req(data, obj_id) {
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
            data.sync(&rq, obj_id);
            Some(CompletionToKernel::new(KernelCompletionData::SyncOkay(
                obj_id,
            )))
        }
        KernelCommand::ObjectDel(obj_id) => {
            tracing::warn!("unimp: object del: {}", obj_id);
            Some(CompletionToKernel::new(KernelCompletionData::Error))
        }
        KernelCommand::ObjectCreate(object_info) => {
            let _ = object_store::unlink_object(object_info.obj_id.raw());
            if object_store::create_object(object_info.obj_id.raw())
                .inspect_err(|e| {
                    tracing::warn!("failed to create object {}: {}", object_info.obj_id, e)
                })
                .is_err()
            {
                Some(CompletionToKernel::new(KernelCompletionData::Error))
            } else {
                Some(CompletionToKernel::new(
                    KernelCompletionData::ObjectInfoCompletion(object_info),
                ))
            }
        }
    }
}
