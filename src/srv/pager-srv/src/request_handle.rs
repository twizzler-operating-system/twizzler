use std::sync::Arc;

use twizzler_abi::pager::{
    CompletionToKernel, KernelCommand, KernelCompletionData, ObjectInfo, ObjectRange, PhysRange,
    RequestFromKernel,
};
use twizzler_object::ObjID;

use crate::data::PagerData;

fn page_data_req(data: Arc<PagerData>, id: ObjID, range: ObjectRange) -> PhysRange {
    return data.fill_mem_page(id, range);
}

fn object_info_req(_data: Arc<PagerData>, id: ObjID) -> ObjectInfo {
    return ObjectInfo::new(id);
}

pub async fn handle_kernel_request(
    request: RequestFromKernel,
    data: Arc<PagerData>,
) -> Option<CompletionToKernel> {
    tracing::debug!("handling kernel request {:?}", request);

    match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range) => {
            tracing::trace!(
                "handling PageDataReq for ObjID: {:?}, Range: start = {}, end = {}",
                obj_id,
                range.start,
                range.end
            );
            let phys_range = page_data_req(data, obj_id, range);
            Some(CompletionToKernel::new(
                KernelCompletionData::PageDataCompletion(obj_id, range, phys_range),
            ))
        }
        KernelCommand::ObjectInfoReq(obj_id) => {
            tracing::trace!("handling ObjectInfo for ObjID: {:?}", obj_id);
            let obj_info = object_info_req(data, obj_id);
            Some(CompletionToKernel::new(
                KernelCompletionData::ObjectInfoCompletion(obj_info),
            ))
        }
        KernelCommand::EchoReq => {
            tracing::trace!("handling EchoReq");
            Some(CompletionToKernel::new(KernelCompletionData::EchoResp))
        }
        KernelCommand::ObjectSync(obj_id) => {
            tracing::warn!("unimp: object sync: {}", obj_id);
            Some(CompletionToKernel::new(KernelCompletionData::SyncOkay(
                obj_id,
            )))
        }
        KernelCommand::ObjectDel(obj_id) => {
            tracing::warn!("unimp: object del: {}", obj_id);
            Some(CompletionToKernel::new(KernelCompletionData::Error))
        }
        KernelCommand::ObjectCreate(object_info) => {
            tracing::warn!("unimp: object create: {}", object_info.obj_id);
            Some(CompletionToKernel::new(KernelCompletionData::Error))
        }
    }
}
