use std::sync::{Arc, Mutex, Weak};

use nvme::{
    ds::queue::{comentry::CommonCompletion, subentry::CommonCommand, QueueId},
    queue::{CompletionQueue, SubmissionQueue},
};
use twizzler_driver::{
    dma::DmaSliceRegion,
    request::{RequestDriver, Requester, ResponseInfo, SubmitRequest},
};

use super::controller::{NvmeController, NvmeControllerRef};

pub struct NvmeQueueDriver {
    subq: Mutex<SubmissionQueue>,
    comq: Mutex<CompletionQueue>,
    controller: Weak<NvmeController>,
    queue_id: QueueId,
}

unsafe impl Send for NvmeQueueDriver {}
unsafe impl Sync for NvmeQueueDriver {}

impl NvmeQueueDriver {
    pub fn new(
        subq: SubmissionQueue,
        comq: CompletionQueue,
        controller: NvmeControllerRef,
        queue_id: QueueId,
    ) -> Self {
        Self {
            subq: Mutex::new(subq),
            comq: Mutex::new(comq),
            controller: Arc::downgrade(&controller),
            queue_id,
        }
    }

    pub async fn check_completions(&self, req: &Requester<Self>) {
        let (new_head, new_bell, resps) = {
            let mut comq = self.comq.lock().unwrap();
            let mut resps = Vec::new();
            let mut new_head = None;
            let mut new_bell = None;
            while let Some((bell, resp)) = comq.get_completion::<CommonCompletion>() {
                let id: u16 = resp.command_id().into();
                resps.push(ResponseInfo::new(resp, id as u64, false));
                new_head = Some(resp.new_sq_head());
                new_bell = Some(bell);
            }
            (new_head, new_bell, resps)
        };
        println!("do we need to be more careful about the head update?");

        if let Some(head) = new_head {
            self.subq.lock().unwrap().update_head(head);
        }

        if let Some(bell) = new_bell {
            if let Some(ctrl) = self.controller.upgrade() {
                ctrl.ring_completion_bell(self.queue_id, bell as u32).await;
            }
        }

        req.finish(&resps);
    }
}

#[async_trait::async_trait]
impl RequestDriver for NvmeQueueDriver {
    type Request = CommonCommand;

    type Response = CommonCompletion;

    type SubmitError = ();

    async fn submit(
        &self,
        reqs: &mut [SubmitRequest<Self::Request>],
    ) -> Result<(), Self::SubmitError> {
        let tail = {
            let mut sq = self.subq.lock().unwrap();
            let mut tail = None;
            for sr in reqs.iter_mut() {
                let cid = (sr.id() as u16).into();
                sr.data_mut().set_cid(cid);
                tail = sq.submit(sr.data());
                assert!(tail.is_some());
            }
            tail
        };
        if let Some(tail) = tail {
            if let Some(ctrl) = self.controller.upgrade() {
                ctrl.ring_submission_bell(self.queue_id, tail as u32).await;
            }
        }
        Ok(())
    }

    fn flush(&self) {}

    fn num_ids(&self) -> usize {
        self.subq.lock().unwrap().len().into()
    }
}

pub struct NvmeQueue {
    requester: Requester<NvmeQueueDriver>,
    sq_reg: DmaSliceRegion<CommonCommand>,
    cq_reg: DmaSliceRegion<CommonCompletion>,
}

impl NvmeQueue {
    pub fn new(
        requester: Requester<NvmeQueueDriver>,
        sq_reg: DmaSliceRegion<CommonCommand>,
        cq_reg: DmaSliceRegion<CommonCompletion>,
    ) -> Self {
        Self {
            requester,
            sq_reg,
            cq_reg,
        }
    }

    pub fn requester(&self) -> &Requester<NvmeQueueDriver> {
        &self.requester
    }

    pub fn submission_dma_region(&mut self) -> &mut DmaSliceRegion<CommonCommand> {
        &mut self.sq_reg
    }

    pub fn completion_dma_region(&mut self) -> &mut DmaSliceRegion<CommonCompletion> {
        &mut self.cq_reg
    }
}

unsafe impl Send for NvmeQueue {}
unsafe impl Sync for NvmeQueue {}
