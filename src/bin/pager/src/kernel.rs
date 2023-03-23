use twizzler_abi::pager::{
    CompletionToKernel, CompletionToPager, KernelCompletionData, KernelRequestError, ObjectInfo,
    ObjectRange, PagerRequest, PagerRequestErr, PhysRange, RequestFromKernel, RequestFromPager,
    NUM_ENTRIES,
};
use twizzler_object::ObjID;
use twizzler_queue::{CallbackQueueReceiver, QueueSender};

use crate::pager::Pager;

pub struct KernelCommandQueue {
    queue: CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>,
}

impl KernelCommandQueue {
    pub fn new(queue: CallbackQueueReceiver<RequestFromKernel, CompletionToKernel>) -> Self { Self { queue } }

    async fn handle_object_info(
        &self,
        id: ObjID,
        pager: &Pager,
    ) -> Result<ObjectInfo, KernelRequestError> {
        todo!()
    }

    async fn handle_page_data(
        &self,
        id: ObjID,
        ranges: [ObjectRange; NUM_ENTRIES],
        pager: &Pager,
    ) -> Result<[PhysRange; NUM_ENTRIES], KernelRequestError> {
        todo!()
    }

    async fn handle_dram_release(&self, amount: usize, pager: &Pager) {
        todo!()
    }

    async fn handle_dram_pages(&self, pages: [PhysRange; NUM_ENTRIES], pager: &Pager) {
        todo!()
    }

    async fn handle_evict_or_sync(
        &self,
        sync: bool,
        id: ObjID,
        ranges: [ObjectRange; NUM_ENTRIES],
        pager: &Pager,
    ) -> Result<(), KernelRequestError> {
        todo!()
    }

    async fn do_handle_request(&self, req: RequestFromKernel, pager: &Pager) -> CompletionToKernel {
        match req.cmd() {
            twizzler_abi::pager::KernelCommand::ObjInfo(id) => {
                match self.handle_object_info(id, pager).await {
                    Ok(info) => CompletionToKernel::new(KernelCompletionData::ObjectInfo(info)),
                    Err(e) => CompletionToKernel::new(KernelCompletionData::Err(e)),
                }
            }
            twizzler_abi::pager::KernelCommand::PageData(id, ranges) => {
                match self.handle_page_data(id, ranges, pager).await {
                    Ok(info) => CompletionToKernel::new(KernelCompletionData::PageInfo(info)),
                    Err(e) => CompletionToKernel::new(KernelCompletionData::Err(e)),
                }
            }
            twizzler_abi::pager::KernelCommand::DramRel(amount) => {
                self.handle_dram_release(amount, pager).await;
                CompletionToKernel::new(KernelCompletionData::Success)
            }
            twizzler_abi::pager::KernelCommand::DramPages(ranges) => {
                self.handle_dram_pages(ranges, pager).await;
                CompletionToKernel::new(KernelCompletionData::Success)
            }
            twizzler_abi::pager::KernelCommand::Evict(id, ranges) => {
                match self.handle_evict_or_sync(false, id, ranges, pager).await {
                    Ok(_) => CompletionToKernel::new(KernelCompletionData::Success),
                    Err(e) => CompletionToKernel::new(KernelCompletionData::Err(e)),
                }
            }
            twizzler_abi::pager::KernelCommand::Sync(id, ranges) => {
                match self.handle_evict_or_sync(true, id, ranges, pager).await {
                    Ok(_) => CompletionToKernel::new(KernelCompletionData::Success),
                    Err(e) => CompletionToKernel::new(KernelCompletionData::Err(e)),
                }
            }
        }
    }

    pub async fn handle_a_request(&self, pager: &Pager) {
        let data = self.queue.receive().await;
        if let Ok(data) = data {
            let comp = self.do_handle_request(data.1, pager).await;
            self.queue.complete(data.0, comp).await.unwrap();
        }
    }
}

pub struct PagerRequestQueue {
    queue: QueueSender<RequestFromPager, CompletionToPager>,
}

impl PagerRequestQueue {
    pub fn new(queue: QueueSender<RequestFromPager, CompletionToPager>) -> Self { Self { queue } }

    pub async fn request_dram(
        &self,
        amount: usize,
    ) -> Result<[PhysRange; NUM_ENTRIES], PagerRequestErr> {
        match self
            .queue
            .submit_and_wait(RequestFromPager::new(PagerRequest::DramReq(amount)))
            .await
            .unwrap()
            .data()
        {
            twizzler_abi::pager::PagerCompletionData::DramPages(ranges) => Ok(ranges),
            twizzler_abi::pager::PagerCompletionData::Err(e) => Err(e),
            _ => Ok(Default::default()),
        }
    }

    pub async fn submit_object_info(&self, info: ObjectInfo) -> Result<(), PagerRequestErr> {
        match self
            .queue
            .submit_and_wait(RequestFromPager::new(PagerRequest::ObjectInfo(info)))
            .await
            .unwrap()
            .data()
        {
            twizzler_abi::pager::PagerCompletionData::Success => Ok(()),
            twizzler_abi::pager::PagerCompletionData::Err(e) => Err(e),
            _ => Err(PagerRequestErr::Unknown),
        }
    }

    pub async fn submit_page_data(
        &self,
        id: ObjID,
        ranges: &[(PhysRange, ObjectRange)],
    ) -> Result<(), PagerRequestErr> {
        let fr = [
            ranges.get(0).unwrap_or(&Default::default()).clone(),
            ranges.get(1).unwrap_or(&Default::default()).clone(),
            ranges.get(2).unwrap_or(&Default::default()).clone(),
            ranges.get(3).unwrap_or(&Default::default()).clone(),
        ];
        match self
            .queue
            .submit_and_wait(RequestFromPager::new(PagerRequest::PageData(id, fr)))
            .await
            .unwrap()
            .data()
        {
            twizzler_abi::pager::PagerCompletionData::Success => Ok(()),
            twizzler_abi::pager::PagerCompletionData::Err(e) => Err(e),
            _ => Err(PagerRequestErr::Unknown),
        }
    }

    pub async fn submit_dram_ranges(&self, ranges: &[PhysRange]) -> Result<(), PagerRequestErr> {
        let fr = [
            ranges.get(0).unwrap_or(&Default::default()).clone(),
            ranges.get(1).unwrap_or(&Default::default()).clone(),
            ranges.get(2).unwrap_or(&Default::default()).clone(),
            ranges.get(3).unwrap_or(&Default::default()).clone(),
        ];
        match self
            .queue
            .submit_and_wait(RequestFromPager::new(PagerRequest::DramPages(fr)))
            .await
            .unwrap()
            .data()
        {
            twizzler_abi::pager::PagerCompletionData::Success => Ok(()),
            twizzler_abi::pager::PagerCompletionData::Err(e) => Err(e),
            _ => Err(PagerRequestErr::Unknown),
        }
    }

    pub async fn submit_evict(
        &self,
        id: ObjID,
        ranges: &[ObjectRange],
    ) -> Result<(), PagerRequestErr> {
        let fr = [
            ranges.get(0).unwrap_or(&Default::default()).clone(),
            ranges.get(1).unwrap_or(&Default::default()).clone(),
            ranges.get(2).unwrap_or(&Default::default()).clone(),
            ranges.get(3).unwrap_or(&Default::default()).clone(),
        ];
        match self
            .queue
            .submit_and_wait(RequestFromPager::new(PagerRequest::Evict(id, fr)))
            .await
            .unwrap()
            .data()
        {
            twizzler_abi::pager::PagerCompletionData::Success => Ok(()),
            twizzler_abi::pager::PagerCompletionData::Err(e) => Err(e),
            _ => Err(PagerRequestErr::Unknown),
        }
    }
}
