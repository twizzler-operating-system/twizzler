use std::sync::Mutex;

use nvme::{
    ds::queue::{comentry::CommonCompletion, subentry::CommonCommand},
    queue::{CompletionQueue, SubmissionQueue},
};
use twizzler_driver::request::{RequestDriver, ResponseInfo, SubmitRequest};
use volatile_cell::VolatileCell;

use super::dma::NvmeDmaSliceRegion;

pub struct NvmeRequester {
    subq: Mutex<SubmissionQueue>,
    comq: Mutex<CompletionQueue>,
    sub_bell: *const VolatileCell<u32>,
    com_bell: *const VolatileCell<u32>,
    _sub_dma: NvmeDmaSliceRegion<CommonCommand>,
    _com_dma: NvmeDmaSliceRegion<CommonCompletion>,
}

unsafe impl Send for NvmeRequester {}
unsafe impl Sync for NvmeRequester {}

impl NvmeRequester {
    pub fn new(
        subq: Mutex<SubmissionQueue>,
        comq: Mutex<CompletionQueue>,
        sub_bell: *const VolatileCell<u32>,
        com_bell: *const VolatileCell<u32>,
        sub_dma: NvmeDmaSliceRegion<CommonCommand>,
        com_dma: NvmeDmaSliceRegion<CommonCompletion>,
    ) -> Self {
        Self {
            subq,
            comq,
            sub_bell,
            com_bell,
            _sub_dma: sub_dma,
            _com_dma: com_dma,
        }
    }

    pub fn check_completions(&self) -> Vec<ResponseInfo<CommonCompletion>> {
        let mut comq = self.comq.lock().unwrap();
        let mut resps = Vec::new();
        let mut new_head = None;
        let mut new_bell = None;
        while let Some((bell, resp)) = comq.get_completion::<CommonCompletion>() {
            let id: u16 = resp.command_id().into();
            resps.push(ResponseInfo::new(resp, id as u64, resp.status().is_error()));
            new_head = Some(resp.new_sq_head());
            new_bell = Some(bell);
        }

        if let Some(head) = new_head {
            self.subq.lock().unwrap().update_head(head);
        }

        if let Some(bell) = new_bell {
            unsafe { self.com_bell.as_ref().unwrap().set(bell as u32) }
        }

        resps
    }
}

#[async_trait::async_trait]
impl RequestDriver for NvmeRequester {
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
            unsafe {
                self.sub_bell.as_ref().unwrap().set(tail as u32);
            }
        }
        Ok(())
    }

    fn flush(&self) {}

    const NUM_IDS: usize = 32;
}
