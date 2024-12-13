use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCompletionData, RequestFromKernel, KernelCommand,
    RequestFromPager, PagerCompletionData, PhysRange, ObjectRange
};

use twizzler_object::{ObjID, Object, ObjectInitFlags, Protections};

use crate::data::PagerData;
use crate::helpers::{physrange_to_pages, page_to_physrange, PAGE};

use std::sync::Arc;

fn page_data_req(data: Arc<PagerData>, id: ObjID, range: ObjectRange) -> PhysRange {
    return data.fill_mem_page(id, range);
}

pub async fn handle_kernel_request(request: RequestFromKernel, data: Arc<PagerData>) -> Option<CompletionToKernel> {
    println!("[pager] handling kernel request {:?}", request);

    match request.cmd() {
        KernelCommand::PageDataReq(obj_id, range) => {
            println!(
                "Handling PageDataReq for ObjID: {:?}, Range: start = {}, end = {}",
                obj_id, range.start, range.end
                );
            let phys_range = page_data_req(data, obj_id, range);
            Some(CompletionToKernel::new(KernelCompletionData::PageDataReq(phys_range)))
        }
        KernelCommand::EchoReq => {
            println!("Handling EchoReq");
            Some(CompletionToKernel::new(KernelCompletionData::EchoResp))
        }
    }
}


