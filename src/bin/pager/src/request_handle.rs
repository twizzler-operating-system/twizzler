use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCompletionData, RequestFromKernel, KernelCommand,
    RequestFromPager, PagerCompletionData, PhysRange, ObjectRange, ObjectInfo
};

use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

use crate::data::PagerData;
use crate::helpers::{physrange_to_pages, page_to_physrange, PAGE};

use std::sync::Arc;

fn page_data_req(data: Arc<PagerData>, id: ObjID, range: ObjectRange) -> PhysRange {
    return data.fill_mem_page(id, range);
}

fn object_info_req(data: Arc<PagerData>, id: ObjID) -> ObjectInfo {
    return ObjectInfo::new(id);
}

pub async fn handle_kernel_request(request: RequestFromKernel, data: Arc<PagerData>) -> Option<CompletionToKernel> {
    println!("[pager] handling kernel request {:?}", request);

    match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range) => {
            println!(
                "[pager] handling PageDataReq for ObjID: {:?}, Range: start = {}, end = {}",
                obj_id, range.start, range.end
                );
            let phys_range = page_data_req(data, obj_id, range);
            Some(CompletionToKernel::new(KernelCompletionData::PageDataCompletion(phys_range)))
        }
        KernelCommand::ObjectInfoReq(obj_id) => {
            println!(
                "[pager] handling ObjectInfo for ObjID: {:?}",
                obj_id
            );
            let obj_info = object_info_req(data, obj_id);
            Some(CompletionToKernel::new(KernelCompletionData::ObjectInfoCompletion(obj_info)))
        }
        KernelCommand::EchoReq => {
            println!("[pager] handling EchoReq");
            Some(CompletionToKernel::new(KernelCompletionData::EchoResp))
        }
    }
}


