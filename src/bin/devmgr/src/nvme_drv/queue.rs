use std::sync::{Arc, Mutex, Weak};

use nvme::{
    ds::queue::{comentry::CommonCompletion, subentry::CommonCommand, QueueId},
    queue::{CompletionQueue, SubmissionQueue},
};
use twizzler_driver::request::{RequestDriver, Requester, ResponseInfo, SubmitRequest};

use super::controller::{NvmeController, NvmeControllerRef};

pub struct NvmeQueueDriver {
    subq: Mutex<SubmissionQueue>,
    comq: Mutex<CompletionQueue>,
    controller: Weak<NvmeController>,
    queue_id: QueueId,
}

unsafe impl Sync for NvmeQueueDriver {}
unsafe impl Send for NvmeQueueDriver {}

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

    pub fn check_completions(&self, req: &Requester<Self>) {
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

        if let Some(head) = new_head {
            self.subq.lock().unwrap().update_head(head);
        }

        if let Some(bell) = new_bell {
            if let Some(ctrl) = self.controller.upgrade() {
                ctrl.ring_completion_bell(self.queue_id, bell as u32);
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
        let mut sq = self.subq.lock().unwrap();
        let mut tail = None;
        for sr in reqs.iter_mut() {
            let cid = (sr.id() as u16).into();
            sr.data_mut().set_cid(cid);
            tail = sq.submit(sr.data());
            assert!(tail.is_some());
        }
        if let Some(tail) = tail {
            if let Some(ctrl) = self.controller.upgrade() {
                ctrl.ring_submission_bell(self.queue_id, tail as u32);
            }
        }
        Ok(())
    }

    fn flush(&self) {}

    const NUM_IDS: usize = 32;
}
